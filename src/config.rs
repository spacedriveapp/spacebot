//! Configuration loading and validation.

use crate::error::{ConfigError, Result};
use crate::llm::routing::RoutingConfig;
use anyhow::Context as _;
use arc_swap::ArcSwap;
use chrono_tz::Tz;
use serde::{Deserialize, Deserializer, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

const CRON_TIMEZONE_ENV_VAR: &str = "SPACEBOT_CRON_TIMEZONE";
const USER_TIMEZONE_ENV_VAR: &str = "SPACEBOT_USER_TIMEZONE";

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
}

impl std::fmt::Debug for ProviderConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProviderConfig")
            .field("api_type", &self.api_type)
            .field("base_url", &self.base_url)
            .field("api_key", &"[REDACTED]")
            .field("name", &self.name)
            .field("use_bearer_auth", &self.use_bearer_auth)
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

const ANTHROPIC_PROVIDER_BASE_URL: &str = "https://api.anthropic.com";
const OPENAI_PROVIDER_BASE_URL: &str = "https://api.openai.com";
const OPENROUTER_PROVIDER_BASE_URL: &str = "https://openrouter.ai/api";
const KILO_PROVIDER_BASE_URL: &str = "https://api.kilo.ai/api/gateway";
const OLLAMA_PROVIDER_BASE_URL: &str = "http://localhost:11434";
const OPENCODE_ZEN_PROVIDER_BASE_URL: &str = "https://opencode.ai/zen";
const OPENCODE_GO_PROVIDER_BASE_URL: &str = "https://opencode.ai/zen/go";
const MINIMAX_PROVIDER_BASE_URL: &str = "https://api.minimax.io/anthropic";
const MINIMAX_CN_PROVIDER_BASE_URL: &str = "https://api.minimaxi.com/anthropic";
const MOONSHOT_PROVIDER_BASE_URL: &str = "https://api.moonshot.ai";

const ZHIPU_PROVIDER_BASE_URL: &str = "https://api.z.ai/api/paas/v4";
const ZAI_CODING_PLAN_BASE_URL: &str = "https://api.z.ai/api/coding/paas/v4";
const DEEPSEEK_PROVIDER_BASE_URL: &str = "https://api.deepseek.com";
const GROQ_PROVIDER_BASE_URL: &str = "https://api.groq.com/openai";
const TOGETHER_PROVIDER_BASE_URL: &str = "https://api.together.xyz";
const XAI_PROVIDER_BASE_URL: &str = "https://api.x.ai";
const MISTRAL_PROVIDER_BASE_URL: &str = "https://api.mistral.ai";
const NVIDIA_PROVIDER_BASE_URL: &str = "https://integrate.api.nvidia.com";
const FIREWORKS_PROVIDER_BASE_URL: &str = "https://api.fireworks.ai/inference";
pub(crate) const GEMINI_PROVIDER_BASE_URL: &str =
    "https://generativelanguage.googleapis.com/v1beta/openai";

/// Returns the default ProviderConfig for a provider ID and API key.
/// Used by API tests and other code that needs provider configs without duplicating metadata.
pub(crate) fn default_provider_config(
    provider_id: &str,
    api_key: impl Into<String>,
) -> Option<ProviderConfig> {
    let api_key = api_key.into();
    Some(match provider_id {
        "anthropic" => ProviderConfig {
            api_type: ApiType::Anthropic,
            base_url: ANTHROPIC_PROVIDER_BASE_URL.to_string(),
            api_key,
            name: None,
            use_bearer_auth: false,
        },
        "openai" => ProviderConfig {
            api_type: ApiType::OpenAiCompletions,
            base_url: OPENAI_PROVIDER_BASE_URL.to_string(),
            api_key,
            name: None,
            use_bearer_auth: false,
        },
        "openrouter" => ProviderConfig {
            api_type: ApiType::OpenAiCompletions,
            base_url: OPENROUTER_PROVIDER_BASE_URL.to_string(),
            api_key,
            name: None,
            use_bearer_auth: false,
        },
        "kilo" => ProviderConfig {
            api_type: ApiType::KiloGateway,
            base_url: KILO_PROVIDER_BASE_URL.to_string(),
            api_key,
            name: Some("Kilo Gateway".to_string()),
            use_bearer_auth: false,
        },
        "zhipu" => ProviderConfig {
            api_type: ApiType::OpenAiChatCompletions,
            base_url: ZHIPU_PROVIDER_BASE_URL.to_string(),
            api_key,
            name: Some("Z.AI (GLM)".to_string()),
            use_bearer_auth: false,
        },
        "groq" => ProviderConfig {
            api_type: ApiType::OpenAiCompletions,
            base_url: GROQ_PROVIDER_BASE_URL.to_string(),
            api_key,
            name: None,
            use_bearer_auth: false,
        },
        "together" => ProviderConfig {
            api_type: ApiType::OpenAiCompletions,
            base_url: TOGETHER_PROVIDER_BASE_URL.to_string(),
            api_key,
            name: None,
            use_bearer_auth: false,
        },
        "fireworks" => ProviderConfig {
            api_type: ApiType::OpenAiCompletions,
            base_url: FIREWORKS_PROVIDER_BASE_URL.to_string(),
            api_key,
            name: None,
            use_bearer_auth: false,
        },
        "deepseek" => ProviderConfig {
            api_type: ApiType::OpenAiCompletions,
            base_url: DEEPSEEK_PROVIDER_BASE_URL.to_string(),
            api_key,
            name: None,
            use_bearer_auth: false,
        },
        "xai" => ProviderConfig {
            api_type: ApiType::OpenAiCompletions,
            base_url: XAI_PROVIDER_BASE_URL.to_string(),
            api_key,
            name: None,
            use_bearer_auth: false,
        },
        "mistral" => ProviderConfig {
            api_type: ApiType::OpenAiCompletions,
            base_url: MISTRAL_PROVIDER_BASE_URL.to_string(),
            api_key,
            name: None,
            use_bearer_auth: false,
        },
        "gemini" => ProviderConfig {
            api_type: ApiType::Gemini,
            base_url: GEMINI_PROVIDER_BASE_URL.to_string(),
            api_key,
            name: None,
            use_bearer_auth: false,
        },
        "ollama" => ProviderConfig {
            api_type: ApiType::OpenAiCompletions,
            base_url: api_key,
            api_key: String::new(),
            name: None,
            use_bearer_auth: false,
        },
        "opencode-zen" => ProviderConfig {
            api_type: ApiType::OpenAiCompletions,
            base_url: OPENCODE_ZEN_PROVIDER_BASE_URL.to_string(),
            api_key,
            name: None,
            use_bearer_auth: false,
        },
        "opencode-go" => ProviderConfig {
            api_type: ApiType::OpenAiCompletions,
            base_url: OPENCODE_GO_PROVIDER_BASE_URL.to_string(),
            api_key,
            name: None,
            use_bearer_auth: false,
        },
        "nvidia" => ProviderConfig {
            api_type: ApiType::OpenAiCompletions,
            base_url: NVIDIA_PROVIDER_BASE_URL.to_string(),
            api_key,
            name: None,
            use_bearer_auth: false,
        },
        "minimax" => ProviderConfig {
            api_type: ApiType::Anthropic,
            base_url: MINIMAX_PROVIDER_BASE_URL.to_string(),
            api_key,
            name: None,
            use_bearer_auth: false,
        },
        "minimax-cn" => ProviderConfig {
            api_type: ApiType::Anthropic,
            base_url: MINIMAX_CN_PROVIDER_BASE_URL.to_string(),
            api_key,
            name: None,
            use_bearer_auth: false,
        },
        "moonshot" => ProviderConfig {
            api_type: ApiType::OpenAiCompletions,
            base_url: MOONSHOT_PROVIDER_BASE_URL.to_string(),
            api_key,
            name: None,
            use_bearer_auth: false,
        },
        "zai-coding-plan" => ProviderConfig {
            api_type: ApiType::OpenAiChatCompletions,
            base_url: ZAI_CODING_PLAN_BASE_URL.to_string(),
            api_key,
            name: Some("Z.AI Coding Plan".to_string()),
            use_bearer_auth: false,
        },
        _ => return None,
    })
}

fn add_shorthand_provider(
    providers: &mut std::collections::HashMap<String, ProviderConfig>,
    provider_id: &str,
    key: Option<String>,
    api_type: ApiType,
    base_url: &str,
    name: Option<&str>,
    use_bearer_auth: bool,
) {
    if let Some(api_key) = key {
        providers
            .entry(provider_id.to_string())
            .or_insert_with(|| ProviderConfig {
                api_type,
                base_url: base_url.to_string(),
                api_key,
                name: name.map(str::to_string),
                use_bearer_auth,
            });
    }
}

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
            .finish()
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

fn evaluate_work_readiness(
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
    pub mcp: Option<Vec<McpServerConfig>>,
    /// Per-agent Brave Search API key override. None inherits from defaults.
    pub brave_search_key: Option<String>,
    /// Optional timezone override for cron active-hours evaluation.
    pub cron_timezone: Option<String>,
    /// Optional timezone override for channel/worker temporal context.
    pub user_timezone: Option<String>,
    /// Sandbox configuration for process containment.
    pub sandbox: Option<crate::sandbox::SandboxConfig>,
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
    pub mcp: Vec<McpServerConfig>,
    pub brave_search_key: Option<String>,
    pub cron_timezone: Option<String>,
    pub user_timezone: Option<String>,
    /// Sandbox configuration for process containment.
    pub sandbox: crate::sandbox::SandboxConfig,
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
            mcp: Vec::new(),
            brave_search_key: None,
            cron_timezone: None,
            user_timezone: None,
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
            mcp: resolve_mcp_configs(&defaults.mcp, self.mcp.as_deref()),
            brave_search_key: self
                .brave_search_key
                .clone()
                .or_else(|| defaults.brave_search_key.clone()),
            cron_timezone: resolved_cron_timezone,
            user_timezone: resolved_user_timezone,
            sandbox: self.sandbox.clone().unwrap_or_default(),
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

/// Normalize an adapter selector: trim whitespace, return `None` if empty.
fn normalize_adapter(adapter: Option<String>) -> Option<String> {
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
    pub chat_id: Option<String>,
    /// Channel IDs this binding applies to. If empty, all channels in the guild/workspace are allowed.
    pub channel_ids: Vec<String>,
    /// Require explicit @mention (or reply-to-bot) for inbound messages.
    pub require_mention: bool,
    /// User IDs allowed to DM the bot through this binding.
    pub dm_allowed_users: Vec<String>,
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
fn binding_adapter_matches(binding: &Binding, message: &crate::InboundMessage) -> bool {
    match (&binding.adapter, message.adapter_selector()) {
        (None, None) => true,
        (Some(expected), Some(actual)) => expected == actual,
        _ => false,
    }
}

#[derive(Debug, Clone)]
struct AdapterValidationState {
    default_present: bool,
    named_instances: std::collections::HashSet<String>,
}

fn is_named_adapter_platform(platform: &str) -> bool {
    matches!(
        platform,
        "discord" | "slack" | "telegram" | "twitch" | "email"
    )
}

fn validate_named_messaging_adapters(
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

fn build_adapter_validation_states(
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

    Ok(states)
}

fn validate_instance_names<'a>(
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

/// Messaging platform credentials (instance-level).
#[derive(Debug, Clone, Default)]
pub struct MessagingConfig {
    pub discord: Option<DiscordConfig>,
    pub slack: Option<SlackConfig>,
    pub telegram: Option<TelegramConfig>,
    pub email: Option<EmailConfig>,
    pub webhook: Option<WebhookConfig>,
    pub twitch: Option<TwitchConfig>,
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

/// Hot-reloadable Discord permission filters.
///
/// Derived from bindings + discord config. Shared with the Discord adapter
/// via `Arc<ArcSwap<..>>` so the file watcher can swap in new values without
/// restarting the gateway connection.
#[derive(Debug, Clone, Default)]
pub struct DiscordPermissions {
    pub guild_filter: Option<Vec<u64>>,
    pub channel_filter: std::collections::HashMap<u64, Vec<u64>>,
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
        Self::from_bindings_for_adapter(slack.dm_allowed_users.clone(), bindings, None)
    }

    /// Build permissions for a named Slack adapter instance.
    pub fn from_instance_config(instance: &SlackInstanceConfig, bindings: &[Binding]) -> Self {
        Self::from_bindings_for_adapter(
            instance.dm_allowed_users.clone(),
            bindings,
            Some(instance.name.as_str()),
        )
    }

    fn from_bindings_for_adapter(
        seed_dm_allowed_users: Vec<String>,
        bindings: &[Binding],
        adapter_selector: Option<&str>,
    ) -> Self {
        let slack_bindings: Vec<&Binding> = bindings
            .iter()
            .filter(|binding| {
                binding.channel == "slack"
                    && binding_adapter_selector_matches(binding, adapter_selector)
            })
            .collect();

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
                if let Some(workspace_id) = &binding.workspace_id
                    && !binding.channel_ids.is_empty()
                {
                    filter
                        .entry(workspace_id.clone())
                        .or_default()
                        .extend(binding.channel_ids.clone());
                }
            }
            filter
        };

        let mut dm_allowed_users = seed_dm_allowed_users;

        for binding in &slack_bindings {
            for id in &binding.dm_allowed_users {
                if !dm_allowed_users.contains(id) {
                    dm_allowed_users.push(id.clone());
                }
            }
        }

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
        Self::from_bindings_for_adapter(
            discord.dm_allowed_users.clone(),
            discord.allow_bot_messages,
            bindings,
            None,
        )
    }

    /// Build permissions for a named Discord adapter instance.
    pub fn from_instance_config(instance: &DiscordInstanceConfig, bindings: &[Binding]) -> Self {
        Self::from_bindings_for_adapter(
            instance.dm_allowed_users.clone(),
            instance.allow_bot_messages,
            bindings,
            Some(instance.name.as_str()),
        )
    }

    fn from_bindings_for_adapter(
        seed_dm_allowed_users: Vec<String>,
        allow_bot_messages: bool,
        bindings: &[Binding],
        adapter_selector: Option<&str>,
    ) -> Self {
        let discord_bindings: Vec<&Binding> = bindings
            .iter()
            .filter(|binding| {
                binding.channel == "discord"
                    && binding_adapter_selector_matches(binding, adapter_selector)
            })
            .collect();

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
                    && !binding.channel_ids.is_empty()
                {
                    let channel_ids: Vec<u64> = binding
                        .channel_ids
                        .iter()
                        .filter_map(|id| id.parse::<u64>().ok())
                        .collect();
                    filter.entry(guild_id).or_default().extend(channel_ids);
                }
            }
            filter
        };

        let mut dm_allowed_users: Vec<u64> = seed_dm_allowed_users
            .iter()
            .filter_map(|id| id.parse::<u64>().ok())
            .collect();

        // Also collect dm_allowed_users from bindings
        for binding in &discord_bindings {
            for id in &binding.dm_allowed_users {
                if let Ok(uid) = id.parse::<u64>()
                    && !dm_allowed_users.contains(&uid)
                {
                    dm_allowed_users.push(uid);
                }
            }
        }

        Self {
            guild_filter,
            channel_filter,
            dm_allowed_users,
            allow_bot_messages,
        }
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
        Self::from_bindings_for_adapter(telegram.dm_allowed_users.clone(), bindings, None)
    }

    /// Build permissions for a named Telegram adapter instance.
    pub fn from_instance_config(instance: &TelegramInstanceConfig, bindings: &[Binding]) -> Self {
        Self::from_bindings_for_adapter(
            instance.dm_allowed_users.clone(),
            bindings,
            Some(instance.name.as_str()),
        )
    }

    fn from_bindings_for_adapter(
        seed_dm_allowed_users: Vec<String>,
        bindings: &[Binding],
        adapter_selector: Option<&str>,
    ) -> Self {
        let telegram_bindings: Vec<&Binding> = bindings
            .iter()
            .filter(|binding| {
                binding.channel == "telegram"
                    && binding_adapter_selector_matches(binding, adapter_selector)
            })
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

        let mut dm_allowed_users: Vec<i64> = seed_dm_allowed_users
            .iter()
            .filter_map(|id| id.parse::<i64>().ok())
            .collect();

        for binding in &telegram_bindings {
            for id in &binding.dm_allowed_users {
                if let Ok(uid) = id.parse::<i64>()
                    && !dm_allowed_users.contains(&uid)
                {
                    dm_allowed_users.push(uid);
                }
            }
        }

        Self {
            chat_filter,
            dm_allowed_users,
        }
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
        Self::from_bindings_for_adapter(bindings, None)
    }

    /// Build permissions for a named Twitch adapter instance.
    pub fn from_instance_config(instance: &TwitchInstanceConfig, bindings: &[Binding]) -> Self {
        Self::from_bindings_for_adapter(bindings, Some(instance.name.as_str()))
    }

    fn from_bindings_for_adapter(bindings: &[Binding], adapter_selector: Option<&str>) -> Self {
        let twitch_bindings: Vec<&Binding> = bindings
            .iter()
            .filter(|binding| {
                binding.channel == "twitch"
                    && binding_adapter_selector_matches(binding, adapter_selector)
            })
            .collect();

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

fn binding_adapter_selector_matches(binding: &Binding, adapter_selector: Option<&str>) -> bool {
    match (binding.adapter.as_deref(), adapter_selector) {
        (None, None) => true,
        (Some(binding_selector), Some(requested_selector)) => {
            binding_selector == requested_selector
        }
        _ => false,
    }
}

#[derive(Debug, Clone)]
pub struct WebhookConfig {
    pub enabled: bool,
    pub port: u16,
    pub bind: String,
    pub auth_token: Option<String>,
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
    links: Vec<TomlLinkDef>,
    #[serde(default)]
    groups: Vec<TomlGroupDef>,
    #[serde(default)]
    humans: Vec<TomlHumanDef>,
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

#[derive(Deserialize)]
struct TomlLinkDef {
    from: String,
    to: String,
    #[serde(default = "default_link_direction")]
    direction: String,
    #[serde(default = "default_link_kind")]
    kind: String,
    /// Backward compat: old configs use `relationship` instead of `kind`
    #[serde(default)]
    relationship: Option<String>,
}

fn default_link_direction() -> String {
    "two_way".into()
}

fn default_link_kind() -> String {
    "peer".into()
}

#[derive(Deserialize)]
struct TomlGroupDef {
    name: String,
    #[serde(default)]
    agent_ids: Vec<String>,
    color: Option<String>,
}

#[derive(Deserialize)]
struct TomlHumanDef {
    id: String,
    display_name: Option<String>,
    role: Option<String>,
    bio: Option<String>,
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
    #[serde(default)]
    auth_token: Option<String>,
}

impl Default for TomlApiConfig {
    fn default() -> Self {
        Self {
            enabled: default_api_enabled(),
            port: default_api_port(),
            bind: default_api_bind(),
            auth_token: None,
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
    kilo_key: Option<String>,
    zhipu_key: Option<String>,
    groq_key: Option<String>,
    together_key: Option<String>,
    fireworks_key: Option<String>,
    deepseek_key: Option<String>,
    xai_key: Option<String>,
    mistral_key: Option<String>,
    gemini_key: Option<String>,
    ollama_key: Option<String>,
    ollama_base_url: Option<String>,
    opencode_zen_key: Option<String>,
    opencode_go_key: Option<String>,
    nvidia_key: Option<String>,
    minimax_key: Option<String>,
    minimax_cn_key: Option<String>,
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
    kilo_key: Option<String>,
    zhipu_key: Option<String>,
    groq_key: Option<String>,
    together_key: Option<String>,
    fireworks_key: Option<String>,
    deepseek_key: Option<String>,
    xai_key: Option<String>,
    mistral_key: Option<String>,
    gemini_key: Option<String>,
    ollama_key: Option<String>,
    ollama_base_url: Option<String>,
    opencode_zen_key: Option<String>,
    opencode_go_key: Option<String>,
    nvidia_key: Option<String>,
    minimax_key: Option<String>,
    minimax_cn_key: Option<String>,
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
            kilo_key: fields.kilo_key,
            zhipu_key: fields.zhipu_key,
            groq_key: fields.groq_key,
            together_key: fields.together_key,
            fireworks_key: fields.fireworks_key,
            deepseek_key: fields.deepseek_key,
            xai_key: fields.xai_key,
            mistral_key: fields.mistral_key,
            gemini_key: fields.gemini_key,
            ollama_key: fields.ollama_key,
            ollama_base_url: fields.ollama_base_url,
            opencode_zen_key: fields.opencode_zen_key,
            opencode_go_key: fields.opencode_go_key,
            nvidia_key: fields.nvidia_key,
            minimax_key: fields.minimax_key,
            minimax_cn_key: fields.minimax_cn_key,
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
    warmup: Option<TomlWarmupConfig>,
    browser: Option<TomlBrowserConfig>,
    #[serde(default)]
    mcp: Vec<TomlMcpServerConfig>,
    brave_search_key: Option<String>,
    cron_timezone: Option<String>,
    user_timezone: Option<String>,
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
    voice: Option<String>,
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
struct TomlWarmupConfig {
    enabled: Option<bool>,
    eager_embedding_load: Option<bool>,
    refresh_secs: Option<u64>,
    startup_delay_secs: Option<u64>,
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
    display_name: Option<String>,
    role: Option<String>,
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
    warmup: Option<TomlWarmupConfig>,
    browser: Option<TomlBrowserConfig>,
    mcp: Option<Vec<TomlMcpServerConfig>>,
    brave_search_key: Option<String>,
    cron_timezone: Option<String>,
    user_timezone: Option<String>,
    sandbox: Option<crate::sandbox::SandboxConfig>,
    #[serde(default)]
    cron: Vec<TomlCronDef>,
}

#[derive(Deserialize)]
struct TomlCronDef {
    id: String,
    prompt: String,
    cron_expr: Option<String>,
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
    email: Option<TomlEmailConfig>,
    webhook: Option<TomlWebhookConfig>,
    twitch: Option<TomlTwitchConfig>,
}

#[derive(Deserialize)]
struct TomlDiscordConfig {
    #[serde(default)]
    enabled: bool,
    token: Option<String>,
    #[serde(default)]
    instances: Vec<TomlDiscordInstanceConfig>,
    #[serde(default)]
    dm_allowed_users: Vec<String>,
    #[serde(default)]
    allow_bot_messages: bool,
}

#[derive(Deserialize)]
struct TomlDiscordInstanceConfig {
    name: String,
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
    instances: Vec<TomlSlackInstanceConfig>,
    #[serde(default)]
    dm_allowed_users: Vec<String>,
    #[serde(default)]
    commands: Vec<TomlSlackCommandConfig>,
}

#[derive(Deserialize)]
struct TomlSlackInstanceConfig {
    name: String,
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
    instances: Vec<TomlTelegramInstanceConfig>,
    #[serde(default)]
    dm_allowed_users: Vec<String>,
}

#[derive(Deserialize)]
struct TomlTelegramInstanceConfig {
    name: String,
    #[serde(default)]
    enabled: bool,
    token: Option<String>,
    #[serde(default)]
    dm_allowed_users: Vec<String>,
}

#[derive(Deserialize)]
struct TomlEmailConfig {
    #[serde(default)]
    enabled: bool,
    imap_host: Option<String>,
    #[serde(default = "default_email_imap_port")]
    imap_port: u16,
    imap_username: Option<String>,
    imap_password: Option<String>,
    #[serde(default = "default_email_imap_use_tls")]
    imap_use_tls: bool,
    smtp_host: Option<String>,
    #[serde(default = "default_email_smtp_port")]
    smtp_port: u16,
    smtp_username: Option<String>,
    smtp_password: Option<String>,
    #[serde(default = "default_email_smtp_use_starttls")]
    smtp_use_starttls: bool,
    from_address: Option<String>,
    from_name: Option<String>,
    #[serde(default = "default_email_poll_interval_secs")]
    poll_interval_secs: u64,
    #[serde(default = "default_email_folders")]
    folders: Vec<String>,
    #[serde(default)]
    allowed_senders: Vec<String>,
    #[serde(default = "default_email_max_body_bytes")]
    max_body_bytes: usize,
    #[serde(default = "default_email_max_attachment_bytes")]
    max_attachment_bytes: usize,
    #[serde(default)]
    instances: Vec<TomlEmailInstanceConfig>,
}

#[derive(Deserialize)]
struct TomlEmailInstanceConfig {
    name: String,
    #[serde(default)]
    enabled: bool,
    imap_host: Option<String>,
    #[serde(default = "default_email_imap_port")]
    imap_port: u16,
    imap_username: Option<String>,
    imap_password: Option<String>,
    #[serde(default = "default_email_imap_use_tls")]
    imap_use_tls: bool,
    smtp_host: Option<String>,
    #[serde(default = "default_email_smtp_port")]
    smtp_port: u16,
    smtp_username: Option<String>,
    smtp_password: Option<String>,
    #[serde(default = "default_email_smtp_use_starttls")]
    smtp_use_starttls: bool,
    from_address: Option<String>,
    from_name: Option<String>,
    #[serde(default = "default_email_poll_interval_secs")]
    poll_interval_secs: u64,
    #[serde(default = "default_email_folders")]
    folders: Vec<String>,
    #[serde(default)]
    allowed_senders: Vec<String>,
    #[serde(default = "default_email_max_body_bytes")]
    max_body_bytes: usize,
    #[serde(default = "default_email_max_attachment_bytes")]
    max_attachment_bytes: usize,
}

#[derive(Deserialize)]
struct TomlWebhookConfig {
    #[serde(default)]
    enabled: bool,
    #[serde(default = "default_webhook_port")]
    port: u16,
    #[serde(default = "default_webhook_bind")]
    bind: String,
    auth_token: Option<String>,
}

#[derive(Deserialize)]
struct TomlTwitchConfig {
    #[serde(default)]
    enabled: bool,
    username: Option<String>,
    oauth_token: Option<String>,
    client_id: Option<String>,
    client_secret: Option<String>,
    refresh_token: Option<String>,
    #[serde(default)]
    instances: Vec<TomlTwitchInstanceConfig>,
    #[serde(default)]
    channels: Vec<String>,
    trigger_prefix: Option<String>,
}

#[derive(Deserialize)]
struct TomlTwitchInstanceConfig {
    name: String,
    #[serde(default)]
    enabled: bool,
    username: Option<String>,
    oauth_token: Option<String>,
    client_id: Option<String>,
    client_secret: Option<String>,
    refresh_token: Option<String>,
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

fn default_email_imap_port() -> u16 {
    993
}

fn default_email_imap_use_tls() -> bool {
    true
}

fn default_email_smtp_port() -> u16 {
    587
}

fn default_email_smtp_use_starttls() -> bool {
    true
}

fn default_email_poll_interval_secs() -> u64 {
    30
}

fn default_email_folders() -> Vec<String> {
    vec!["INBOX".to_string()]
}

fn default_email_max_body_bytes() -> usize {
    256 * 1024
}

fn default_email_max_attachment_bytes() -> usize {
    10 * 1024 * 1024
}

#[derive(Deserialize)]
struct TomlBinding {
    agent_id: String,
    channel: String,
    #[serde(default)]
    adapter: Option<String>,
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
            Err(ConfigError::Invalid(
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
        voice: t.voice.unwrap_or_else(|| base.voice.clone()),
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
        if crate::auth::credentials_path(&instance_dir).exists()
            || crate::openai_auth::credentials_path(&instance_dir).exists()
        {
            return false;
        }

        // Check if we have any legacy env keys configured
        let has_legacy_keys = std::env::var("ANTHROPIC_API_KEY").is_ok()
            || std::env::var("OPENAI_API_KEY").is_ok()
            || std::env::var("OPENROUTER_API_KEY").is_ok()
            || std::env::var("KILO_API_KEY").is_ok()
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
            || std::env::var("OPENCODE_GO_API_KEY").is_ok()
            || std::env::var("MINIMAX_API_KEY").is_ok()
            || std::env::var("MINIMAX_CN_API_KEY").is_ok()
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
            || std::env::var("KILO_API_KEY").is_ok()
            || std::env::var("OPENCODE_ZEN_API_KEY").is_ok()
            || std::env::var("OPENCODE_GO_API_KEY").is_ok()
            || std::env::var("MINIMAX_CN_API_KEY").is_ok();

        !has_provider_env_vars && !has_legacy_bootstrap_vars
    }

    /// Load configuration from the default config file, falling back to env vars.
    pub fn load() -> Result<Self> {
        let instance_dir = Self::default_instance_dir();

        Self::load_for_instance(&instance_dir)
    }

    /// Load configuration for a specific instance directory.
    pub fn load_for_instance(instance_dir: &Path) -> Result<Self> {
        let config_path = instance_dir.join("config.toml");

        if config_path.exists() {
            Self::load_from_path(&config_path)
        } else {
            Self::load_from_env(instance_dir)
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
        let anthropic_from_auth_token = std::env::var("ANTHROPIC_API_KEY").is_err()
            && std::env::var("ANTHROPIC_AUTH_TOKEN").is_ok();
        let mut llm = LlmConfig {
            anthropic_key: std::env::var("ANTHROPIC_API_KEY")
                .ok()
                .or_else(|| std::env::var("ANTHROPIC_AUTH_TOKEN").ok()),
            openai_key: std::env::var("OPENAI_API_KEY").ok(),
            openrouter_key: std::env::var("OPENROUTER_API_KEY").ok(),
            kilo_key: std::env::var("KILO_API_KEY").ok(),
            zhipu_key: std::env::var("ZHIPU_API_KEY").ok(),
            groq_key: std::env::var("GROQ_API_KEY").ok(),
            together_key: std::env::var("TOGETHER_API_KEY").ok(),
            fireworks_key: std::env::var("FIREWORKS_API_KEY").ok(),
            deepseek_key: std::env::var("DEEPSEEK_API_KEY").ok(),
            xai_key: std::env::var("XAI_API_KEY").ok(),
            mistral_key: std::env::var("MISTRAL_API_KEY").ok(),
            gemini_key: std::env::var("GEMINI_API_KEY").ok(),
            ollama_key: std::env::var("OLLAMA_API_KEY").ok(),
            ollama_base_url: std::env::var("OLLAMA_BASE_URL").ok(),
            opencode_zen_key: std::env::var("OPENCODE_ZEN_API_KEY").ok(),
            opencode_go_key: std::env::var("OPENCODE_GO_API_KEY").ok(),
            nvidia_key: std::env::var("NVIDIA_API_KEY").ok(),
            minimax_key: std::env::var("MINIMAX_API_KEY").ok(),
            minimax_cn_key: std::env::var("MINIMAX_CN_API_KEY").ok(),
            moonshot_key: std::env::var("MOONSHOT_API_KEY").ok(),
            zai_coding_plan_key: std::env::var("ZAI_CODING_PLAN_API_KEY").ok(),
            providers: HashMap::new(),
        };

        // Populate providers from env vars (same as from_toml does)
        if let Some(anthropic_key) = llm.anthropic_key.clone() {
            let base_url = std::env::var("ANTHROPIC_BASE_URL")
                .unwrap_or_else(|_| ANTHROPIC_PROVIDER_BASE_URL.to_string());
            llm.providers
                .entry("anthropic".to_string())
                .or_insert_with(|| ProviderConfig {
                    api_type: ApiType::Anthropic,
                    base_url,
                    api_key: anthropic_key,
                    name: None,
                    use_bearer_auth: anthropic_from_auth_token,
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
                    use_bearer_auth: false,
                });
        }

        add_shorthand_provider(
            &mut llm.providers,
            "kilo",
            llm.kilo_key.clone(),
            ApiType::KiloGateway,
            KILO_PROVIDER_BASE_URL,
            Some("Kilo Gateway"),
            false,
        );
        add_shorthand_provider(
            &mut llm.providers,
            "zhipu",
            llm.zhipu_key.clone(),
            ApiType::OpenAiChatCompletions,
            ZHIPU_PROVIDER_BASE_URL,
            Some("Z.AI (GLM)"),
            false,
        );
        add_shorthand_provider(
            &mut llm.providers,
            "zai-coding-plan",
            llm.zai_coding_plan_key.clone(),
            ApiType::OpenAiChatCompletions,
            ZAI_CODING_PLAN_BASE_URL,
            Some("Z.AI Coding Plan"),
            false,
        );

        add_shorthand_provider(
            &mut llm.providers,
            "opencode-zen",
            llm.opencode_zen_key.clone(),
            ApiType::OpenAiCompletions,
            OPENCODE_ZEN_PROVIDER_BASE_URL,
            None,
            false,
        );

        add_shorthand_provider(
            &mut llm.providers,
            "opencode-go",
            llm.opencode_go_key.clone(),
            ApiType::OpenAiCompletions,
            OPENCODE_GO_PROVIDER_BASE_URL,
            None,
            false,
        );

        if let Some(minimax_key) = llm.minimax_key.clone() {
            llm.providers
                .entry("minimax".to_string())
                .or_insert_with(|| ProviderConfig {
                    api_type: ApiType::Anthropic,
                    base_url: MINIMAX_PROVIDER_BASE_URL.to_string(),
                    api_key: minimax_key,
                    name: None,
                    use_bearer_auth: false,
                });
        }

        if let Some(minimax_cn_key) = llm.minimax_cn_key.clone() {
            llm.providers
                .entry("minimax-cn".to_string())
                .or_insert_with(|| ProviderConfig {
                    api_type: ApiType::Anthropic,
                    base_url: MINIMAX_CN_PROVIDER_BASE_URL.to_string(),
                    api_key: minimax_cn_key,
                    name: None,
                    use_bearer_auth: false,
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
                    use_bearer_auth: false,
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
                    use_bearer_auth: false,
                });
        }

        add_shorthand_provider(
            &mut llm.providers,
            "kilo",
            llm.kilo_key.clone(),
            ApiType::KiloGateway,
            KILO_PROVIDER_BASE_URL,
            Some("Kilo Gateway"),
            false,
        );
        add_shorthand_provider(
            &mut llm.providers,
            "zhipu",
            llm.zhipu_key.clone(),
            ApiType::OpenAiChatCompletions,
            ZHIPU_PROVIDER_BASE_URL,
            Some("Z.AI (GLM)"),
            false,
        );
        add_shorthand_provider(
            &mut llm.providers,
            "zai-coding-plan",
            llm.zai_coding_plan_key.clone(),
            ApiType::OpenAiChatCompletions,
            ZAI_CODING_PLAN_BASE_URL,
            Some("Z.AI Coding Plan"),
            false,
        );

        if let Some(opencode_zen_key) = llm.opencode_zen_key.clone() {
            llm.providers
                .entry("opencode-zen".to_string())
                .or_insert_with(|| ProviderConfig {
                    api_type: ApiType::OpenAiCompletions,
                    base_url: OPENCODE_ZEN_PROVIDER_BASE_URL.to_string(),
                    api_key: opencode_zen_key,
                    name: None,
                    use_bearer_auth: false,
                });
        }

        if let Some(opencode_go_key) = llm.opencode_go_key.clone() {
            llm.providers
                .entry("opencode-go".to_string())
                .or_insert_with(|| ProviderConfig {
                    api_type: ApiType::OpenAiCompletions,
                    base_url: OPENCODE_GO_PROVIDER_BASE_URL.to_string(),
                    api_key: opencode_go_key,
                    name: None,
                    use_bearer_auth: false,
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
                    use_bearer_auth: false,
                });
        }

        if let Some(minimax_cn_key) = llm.minimax_cn_key.clone() {
            llm.providers
                .entry("minimax-cn".to_string())
                .or_insert_with(|| ProviderConfig {
                    api_type: ApiType::Anthropic,
                    base_url: MINIMAX_CN_PROVIDER_BASE_URL.to_string(),
                    api_key: minimax_cn_key,
                    name: None,
                    use_bearer_auth: false,
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
                    use_bearer_auth: false,
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
                    use_bearer_auth: false,
                });
        }

        if let Some(fireworks_key) = llm.fireworks_key.clone() {
            llm.providers
                .entry("fireworks".to_string())
                .or_insert_with(|| ProviderConfig {
                    api_type: ApiType::OpenAiCompletions,
                    base_url: FIREWORKS_PROVIDER_BASE_URL.to_string(),
                    api_key: fireworks_key,
                    name: None,
                    use_bearer_auth: false,
                });
        }

        if let Some(deepseek_key) = llm.deepseek_key.clone() {
            llm.providers
                .entry("deepseek".to_string())
                .or_insert_with(|| ProviderConfig {
                    api_type: ApiType::OpenAiCompletions,
                    base_url: DEEPSEEK_PROVIDER_BASE_URL.to_string(),
                    api_key: deepseek_key,
                    name: None,
                    use_bearer_auth: false,
                });
        }

        if let Some(gemini_key) = llm.gemini_key.clone() {
            llm.providers
                .entry("gemini".to_string())
                .or_insert_with(|| ProviderConfig {
                    api_type: ApiType::Gemini,
                    base_url: GEMINI_PROVIDER_BASE_URL.to_string(),
                    api_key: gemini_key,
                    name: None,
                    use_bearer_auth: false,
                });
        }

        if let Some(groq_key) = llm.groq_key.clone() {
            llm.providers
                .entry("groq".to_string())
                .or_insert_with(|| ProviderConfig {
                    api_type: ApiType::OpenAiCompletions,
                    base_url: GROQ_PROVIDER_BASE_URL.to_string(),
                    api_key: groq_key,
                    name: None,
                    use_bearer_auth: false,
                });
        }

        if let Some(together_key) = llm.together_key.clone() {
            llm.providers
                .entry("together".to_string())
                .or_insert_with(|| ProviderConfig {
                    api_type: ApiType::OpenAiCompletions,
                    base_url: TOGETHER_PROVIDER_BASE_URL.to_string(),
                    api_key: together_key,
                    name: None,
                    use_bearer_auth: false,
                });
        }

        if let Some(xai_key) = llm.xai_key.clone() {
            llm.providers
                .entry("xai".to_string())
                .or_insert_with(|| ProviderConfig {
                    api_type: ApiType::OpenAiCompletions,
                    base_url: XAI_PROVIDER_BASE_URL.to_string(),
                    api_key: xai_key,
                    name: None,
                    use_bearer_auth: false,
                });
        }

        if let Some(mistral_key) = llm.mistral_key.clone() {
            llm.providers
                .entry("mistral".to_string())
                .or_insert_with(|| ProviderConfig {
                    api_type: ApiType::OpenAiCompletions,
                    base_url: MISTRAL_PROVIDER_BASE_URL.to_string(),
                    api_key: mistral_key,
                    name: None,
                    use_bearer_auth: false,
                });
        }

        if llm.ollama_base_url.is_some() || llm.ollama_key.is_some() {
            llm.providers
                .entry("ollama".to_string())
                .or_insert_with(|| ProviderConfig {
                    api_type: ApiType::OpenAiCompletions,
                    base_url: llm
                        .ollama_base_url
                        .clone()
                        .unwrap_or_else(|| OLLAMA_PROVIDER_BASE_URL.to_string()),
                    api_key: llm.ollama_key.clone().unwrap_or_default(),
                    name: None,
                    use_bearer_auth: false,
                });
        }

        // Note: We allow boot without provider keys now. System starts in setup mode.
        // Agents are initialized later when keys are added via API.

        // Env-only routing: check for env overrides on channel/worker models.
        // SPACEBOT_MODEL overrides all process types at once; specific vars take precedence.
        // ANTHROPIC_MODEL sets all anthropic/* models to the specified value.
        let mut routing = RoutingConfig::default();
        if let Ok(model) = std::env::var("SPACEBOT_MODEL") {
            routing.channel = model.clone();
            routing.branch = model.clone();
            routing.worker = model.clone();
            routing.compactor = model.clone();
            routing.cortex = model;
        }
        if let Ok(anthropic_model) = std::env::var("ANTHROPIC_MODEL") {
            // ANTHROPIC_MODEL sets all anthropic/* routes to the specified model
            let channel = format!("anthropic/{}", anthropic_model);
            let branch = format!("anthropic/{}", anthropic_model);
            let worker = format!("anthropic/{}", anthropic_model);
            let compactor = format!("anthropic/{}", anthropic_model);
            let cortex = format!("anthropic/{}", anthropic_model);
            routing.channel = channel;
            routing.branch = branch;
            routing.worker = worker;
            routing.compactor = compactor;
            routing.cortex = cortex;
        }
        if let Ok(channel_model) = std::env::var("SPACEBOT_CHANNEL_MODEL") {
            routing.channel = channel_model;
        }
        if let Ok(worker_model) = std::env::var("SPACEBOT_WORKER_MODEL") {
            routing.worker = worker_model;
        }
        if let Ok(voice_model) = std::env::var("SPACEBOT_VOICE_MODEL") {
            routing.voice = voice_model;
        }

        let agents = vec![AgentConfig {
            id: "main".into(),
            default: true,
            display_name: None,
            role: None,
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
            warmup: None,
            browser: None,
            mcp: None,
            brave_search_key: None,
            cron_timezone: None,
            user_timezone: None,
            sandbox: None,
            cron: Vec::new(),
        }];

        let mut api = ApiConfig::default();
        api.bind = hosted_api_bind(api.bind);

        Ok(Self {
            instance_dir: instance_dir.to_path_buf(),
            llm,
            defaults: DefaultsConfig::default(),
            agents,
            links: Vec::new(),
            groups: Vec::new(),
            humans: vec![HumanDef {
                id: "admin".into(),
                display_name: None,
                role: None,
                bio: None,
            }],
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

        let toml_llm_anthropic_key_was_none = toml
            .llm
            .anthropic_key
            .as_deref()
            .and_then(resolve_env_value)
            .is_none();

        let mut llm = LlmConfig {
            anthropic_key: toml
                .llm
                .anthropic_key
                .as_deref()
                .and_then(resolve_env_value)
                .or_else(|| std::env::var("ANTHROPIC_API_KEY").ok())
                .or_else(|| std::env::var("ANTHROPIC_AUTH_TOKEN").ok()),
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
            kilo_key: std::env::var("KILO_API_KEY")
                .ok()
                .or_else(|| toml.llm.kilo_key.as_deref().and_then(resolve_env_value)),
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
            gemini_key: toml
                .llm
                .gemini_key
                .as_deref()
                .and_then(resolve_env_value)
                .or_else(|| std::env::var("GEMINI_API_KEY").ok()),
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
            opencode_go_key: std::env::var("OPENCODE_GO_API_KEY").ok().or_else(|| {
                toml.llm
                    .opencode_go_key
                    .as_deref()
                    .and_then(resolve_env_value)
            }),
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
            minimax_cn_key: toml
                .llm
                .minimax_cn_key
                .as_deref()
                .and_then(resolve_env_value)
                .or_else(|| std::env::var("MINIMAX_CN_API_KEY").ok()),
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
                    let api_key = resolve_env_value(&config.api_key).ok_or_else(|| {
                        anyhow::anyhow!("failed to resolve API key for provider '{}'", provider_id)
                    })?;
                    Ok((
                        provider_id.to_lowercase(),
                        ProviderConfig {
                            api_type: config.api_type,
                            base_url: config.base_url,
                            api_key,
                            name: config.name,
                            use_bearer_auth: false,
                        },
                    ))
                })
                .collect::<anyhow::Result<_>>()?,
        };

        // Detect if the Anthropic key came from ANTHROPIC_AUTH_TOKEN (proxy auth).
        // In from_toml, the key may come from toml config, ANTHROPIC_API_KEY, or
        // ANTHROPIC_AUTH_TOKEN (in that priority order). We only set use_bearer_auth
        // if AUTH_TOKEN was the actual source.
        let anthropic_from_auth_token = toml_llm_anthropic_key_was_none
            && std::env::var("ANTHROPIC_API_KEY").is_err()
            && std::env::var("ANTHROPIC_AUTH_TOKEN").is_ok();

        if let Some(anthropic_key) = llm.anthropic_key.clone() {
            let base_url = std::env::var("ANTHROPIC_BASE_URL")
                .unwrap_or_else(|_| ANTHROPIC_PROVIDER_BASE_URL.to_string());
            llm.providers
                .entry("anthropic".to_string())
                .or_insert_with(|| ProviderConfig {
                    api_type: ApiType::Anthropic,
                    base_url,
                    api_key: anthropic_key,
                    name: None,
                    use_bearer_auth: anthropic_from_auth_token,
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
                    use_bearer_auth: false,
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
                    use_bearer_auth: false,
                });
        }

        add_shorthand_provider(
            &mut llm.providers,
            "kilo",
            llm.kilo_key.clone(),
            ApiType::KiloGateway,
            KILO_PROVIDER_BASE_URL,
            Some("Kilo Gateway"),
            false,
        );
        add_shorthand_provider(
            &mut llm.providers,
            "zhipu",
            llm.zhipu_key.clone(),
            ApiType::OpenAiChatCompletions,
            ZHIPU_PROVIDER_BASE_URL,
            Some("Z.AI (GLM)"),
            false,
        );
        add_shorthand_provider(
            &mut llm.providers,
            "zai-coding-plan",
            llm.zai_coding_plan_key.clone(),
            ApiType::OpenAiChatCompletions,
            ZAI_CODING_PLAN_BASE_URL,
            Some("Z.AI Coding Plan"),
            false,
        );

        add_shorthand_provider(
            &mut llm.providers,
            "opencode-zen",
            llm.opencode_zen_key.clone(),
            ApiType::OpenAiCompletions,
            OPENCODE_ZEN_PROVIDER_BASE_URL,
            None,
            false,
        );

        add_shorthand_provider(
            &mut llm.providers,
            "opencode-go",
            llm.opencode_go_key.clone(),
            ApiType::OpenAiCompletions,
            OPENCODE_GO_PROVIDER_BASE_URL,
            None,
            false,
        );

        if let Some(minimax_key) = llm.minimax_key.clone() {
            llm.providers
                .entry("minimax".to_string())
                .or_insert_with(|| ProviderConfig {
                    api_type: ApiType::Anthropic,
                    base_url: MINIMAX_PROVIDER_BASE_URL.to_string(),
                    api_key: minimax_key,
                    name: None,
                    use_bearer_auth: false,
                });
        }

        if let Some(minimax_cn_key) = llm.minimax_cn_key.clone() {
            llm.providers
                .entry("minimax-cn".to_string())
                .or_insert_with(|| ProviderConfig {
                    api_type: ApiType::Anthropic,
                    base_url: MINIMAX_CN_PROVIDER_BASE_URL.to_string(),
                    api_key: minimax_cn_key,
                    name: None,
                    use_bearer_auth: false,
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
                    use_bearer_auth: false,
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
                    use_bearer_auth: false,
                });
        }

        if let Some(fireworks_key) = llm.fireworks_key.clone() {
            llm.providers
                .entry("fireworks".to_string())
                .or_insert_with(|| ProviderConfig {
                    api_type: ApiType::OpenAiCompletions,
                    base_url: FIREWORKS_PROVIDER_BASE_URL.to_string(),
                    api_key: fireworks_key,
                    name: None,
                    use_bearer_auth: false,
                });
        }

        if let Some(deepseek_key) = llm.deepseek_key.clone() {
            llm.providers
                .entry("deepseek".to_string())
                .or_insert_with(|| ProviderConfig {
                    api_type: ApiType::OpenAiCompletions,
                    base_url: DEEPSEEK_PROVIDER_BASE_URL.to_string(),
                    api_key: deepseek_key,
                    name: None,
                    use_bearer_auth: false,
                });
        }

        if let Some(gemini_key) = llm.gemini_key.clone() {
            llm.providers
                .entry("gemini".to_string())
                .or_insert_with(|| ProviderConfig {
                    api_type: ApiType::Gemini,
                    base_url: GEMINI_PROVIDER_BASE_URL.to_string(),
                    api_key: gemini_key,
                    name: None,
                    use_bearer_auth: false,
                });
        }

        if let Some(groq_key) = llm.groq_key.clone() {
            llm.providers
                .entry("groq".to_string())
                .or_insert_with(|| ProviderConfig {
                    api_type: ApiType::OpenAiCompletions,
                    base_url: GROQ_PROVIDER_BASE_URL.to_string(),
                    api_key: groq_key,
                    name: None,
                    use_bearer_auth: false,
                });
        }

        if let Some(together_key) = llm.together_key.clone() {
            llm.providers
                .entry("together".to_string())
                .or_insert_with(|| ProviderConfig {
                    api_type: ApiType::OpenAiCompletions,
                    base_url: TOGETHER_PROVIDER_BASE_URL.to_string(),
                    api_key: together_key,
                    name: None,
                    use_bearer_auth: false,
                });
        }

        if let Some(xai_key) = llm.xai_key.clone() {
            llm.providers
                .entry("xai".to_string())
                .or_insert_with(|| ProviderConfig {
                    api_type: ApiType::OpenAiCompletions,
                    base_url: XAI_PROVIDER_BASE_URL.to_string(),
                    api_key: xai_key,
                    name: None,
                    use_bearer_auth: false,
                });
        }

        if let Some(mistral_key) = llm.mistral_key.clone() {
            llm.providers
                .entry("mistral".to_string())
                .or_insert_with(|| ProviderConfig {
                    api_type: ApiType::OpenAiCompletions,
                    base_url: MISTRAL_PROVIDER_BASE_URL.to_string(),
                    api_key: mistral_key,
                    name: None,
                    use_bearer_auth: false,
                });
        }

        if llm.ollama_base_url.is_some() || llm.ollama_key.is_some() {
            llm.providers
                .entry("ollama".to_string())
                .or_insert_with(|| ProviderConfig {
                    api_type: ApiType::OpenAiCompletions,
                    base_url: llm
                        .ollama_base_url
                        .clone()
                        .unwrap_or_else(|| OLLAMA_PROVIDER_BASE_URL.to_string()),
                    api_key: llm.ollama_key.clone().unwrap_or_default(),
                    name: None,
                    use_bearer_auth: false,
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
            warmup: toml
                .defaults
                .warmup
                .map(|w| WarmupConfig {
                    enabled: w.enabled.unwrap_or(base_defaults.warmup.enabled),
                    eager_embedding_load: w
                        .eager_embedding_load
                        .unwrap_or(base_defaults.warmup.eager_embedding_load),
                    refresh_secs: w.refresh_secs.unwrap_or(base_defaults.warmup.refresh_secs),
                    startup_delay_secs: w
                        .startup_delay_secs
                        .unwrap_or(base_defaults.warmup.startup_delay_secs),
                })
                .unwrap_or(base_defaults.warmup),
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
            cron_timezone: toml
                .defaults
                .cron_timezone
                .as_deref()
                .and_then(resolve_env_value),
            user_timezone: toml
                .defaults
                .user_timezone
                .as_deref()
                .and_then(resolve_env_value),
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
                        cron_expr: h.cron_expr,
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
                    display_name: a.display_name,
                    role: a.role,
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
                    warmup: a.warmup.map(|w| WarmupConfig {
                        enabled: w.enabled.unwrap_or(defaults.warmup.enabled),
                        eager_embedding_load: w
                            .eager_embedding_load
                            .unwrap_or(defaults.warmup.eager_embedding_load),
                        refresh_secs: w.refresh_secs.unwrap_or(defaults.warmup.refresh_secs),
                        startup_delay_secs: w
                            .startup_delay_secs
                            .unwrap_or(defaults.warmup.startup_delay_secs),
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
                    cron_timezone: a.cron_timezone.as_deref().and_then(resolve_env_value),
                    user_timezone: a.user_timezone.as_deref().and_then(resolve_env_value),
                    sandbox: a.sandbox,
                    cron,
                })
            })
            .collect::<Result<Vec<_>>>()?;

        if agents.is_empty() {
            agents.push(AgentConfig {
                id: "main".into(),
                default: true,
                display_name: None,
                role: None,
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
                warmup: None,
                browser: None,
                mcp: None,
                brave_search_key: None,
                cron_timezone: None,
                user_timezone: None,
                sandbox: None,
                cron: Vec::new(),
            });
        }

        if !agents.iter().any(|a| a.default)
            && let Some(first) = agents.first_mut()
        {
            first.default = true;
        }

        let messaging = MessagingConfig {
            discord: toml.messaging.discord.and_then(|d| {
                let instances = d
                    .instances
                    .into_iter()
                    .map(|instance| {
                        let token = instance.token.as_deref().and_then(resolve_env_value);
                        if instance.enabled && token.is_none() {
                            tracing::warn!(
                                adapter = %instance.name,
                                "discord instance is enabled but token is missing/unresolvable — disabling"
                            );
                        }
                        DiscordInstanceConfig {
                            name: instance.name,
                            enabled: instance.enabled && token.is_some(),
                            token: token.unwrap_or_default(),
                            dm_allowed_users: instance.dm_allowed_users,
                            allow_bot_messages: instance.allow_bot_messages,
                        }
                    })
                    .collect::<Vec<_>>();

                let token = std::env::var("DISCORD_BOT_TOKEN")
                    .ok()
                    .or_else(|| d.token.as_deref().and_then(resolve_env_value));

                if token.is_none() && instances.is_empty() {
                    return None;
                }

                Some(DiscordConfig {
                    enabled: d.enabled,
                    token: token.unwrap_or_default(),
                    instances,
                    dm_allowed_users: d.dm_allowed_users,
                    allow_bot_messages: d.allow_bot_messages,
                })
            }),
            slack: toml.messaging.slack.and_then(|s| {
                let instances = s
                    .instances
                    .into_iter()
                    .map(|instance| {
                        let bot_token =
                            instance.bot_token.as_deref().and_then(resolve_env_value);
                        let app_token =
                            instance.app_token.as_deref().and_then(resolve_env_value);
                        if instance.enabled && (bot_token.is_none() || app_token.is_none()) {
                            tracing::warn!(
                                adapter = %instance.name,
                                "slack instance is enabled but tokens are missing/unresolvable — disabling"
                            );
                        }
                        let has_credentials = bot_token.is_some() && app_token.is_some();
                        SlackInstanceConfig {
                            name: instance.name,
                            enabled: instance.enabled && has_credentials,
                            bot_token: bot_token.unwrap_or_default(),
                            app_token: app_token.unwrap_or_default(),
                            dm_allowed_users: instance.dm_allowed_users,
                            commands: instance
                                .commands
                                .into_iter()
                                .map(|command| SlackCommandConfig {
                                    command: command.command,
                                    agent_id: command.agent_id,
                                    description: command.description,
                                })
                                .collect(),
                        }
                    })
                    .collect::<Vec<_>>();

                let bot_token = std::env::var("SLACK_BOT_TOKEN")
                    .ok()
                    .or_else(|| s.bot_token.as_deref().and_then(resolve_env_value));
                let app_token = std::env::var("SLACK_APP_TOKEN")
                    .ok()
                    .or_else(|| s.app_token.as_deref().and_then(resolve_env_value));

                if (bot_token.is_none() || app_token.is_none()) && instances.is_empty() {
                    return None;
                }

                Some(SlackConfig {
                    enabled: s.enabled,
                    bot_token: bot_token.unwrap_or_default(),
                    app_token: app_token.unwrap_or_default(),
                    instances,
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
                let instances = t
                    .instances
                    .into_iter()
                    .map(|instance| {
                        let token = instance.token.as_deref().and_then(resolve_env_value);
                        if instance.enabled && token.is_none() {
                            tracing::warn!(
                                adapter = %instance.name,
                                "telegram instance is enabled but token is missing/unresolvable — disabling"
                            );
                        }
                        TelegramInstanceConfig {
                            name: instance.name,
                            enabled: instance.enabled && token.is_some(),
                            token: token.unwrap_or_default(),
                            dm_allowed_users: instance.dm_allowed_users,
                        }
                    })
                    .collect::<Vec<_>>();

                let token = std::env::var("TELEGRAM_BOT_TOKEN")
                    .ok()
                    .or_else(|| t.token.as_deref().and_then(resolve_env_value));

                if token.is_none() && instances.is_empty() {
                    return None;
                }

                Some(TelegramConfig {
                    enabled: t.enabled,
                    token: token.unwrap_or_default(),
                    instances,
                    dm_allowed_users: t.dm_allowed_users,
                })
            }),
            email: toml.messaging.email.and_then(|email| {
                let instances = email
                    .instances
                    .into_iter()
                    .map(|instance| {
                        let imap_host =
                            instance.imap_host.as_deref().and_then(resolve_env_value);
                        let imap_username =
                            instance.imap_username.as_deref().and_then(resolve_env_value);
                        let imap_password =
                            instance.imap_password.as_deref().and_then(resolve_env_value);
                        let smtp_host =
                            instance.smtp_host.as_deref().and_then(resolve_env_value);

                        let has_credentials = imap_host.is_some()
                            && imap_username.is_some()
                            && imap_password.is_some()
                            && smtp_host.is_some();

                        if instance.enabled && !has_credentials {
                            tracing::warn!(
                                adapter = %instance.name,
                                "email instance is enabled but credentials are missing/unresolvable — disabling"
                            );
                        }

                        let imap_username_val = imap_username.unwrap_or_default();
                        let imap_password_val = imap_password.unwrap_or_default();
                        let smtp_username = instance
                            .smtp_username
                            .as_deref()
                            .and_then(resolve_env_value)
                            .unwrap_or_else(|| imap_username_val.clone());
                        let smtp_password = instance
                            .smtp_password
                            .as_deref()
                            .and_then(resolve_env_value)
                            .unwrap_or_else(|| imap_password_val.clone());
                        let from_address = instance
                            .from_address
                            .as_deref()
                            .and_then(resolve_env_value)
                            .unwrap_or_else(|| smtp_username.clone());
                        let from_name =
                            instance.from_name.as_deref().and_then(resolve_env_value);

                        EmailInstanceConfig {
                            name: instance.name,
                            enabled: instance.enabled && has_credentials,
                            imap_host: imap_host.unwrap_or_default(),
                            imap_port: instance.imap_port,
                            imap_username: imap_username_val,
                            imap_password: imap_password_val,
                            imap_use_tls: instance.imap_use_tls,
                            smtp_host: smtp_host.unwrap_or_default(),
                            smtp_port: instance.smtp_port,
                            smtp_username,
                            smtp_password,
                            smtp_use_starttls: instance.smtp_use_starttls,
                            from_address,
                            from_name,
                            poll_interval_secs: instance.poll_interval_secs,
                            folders: if instance.folders.is_empty() {
                                vec!["INBOX".to_string()]
                            } else {
                                instance.folders
                            },
                            allowed_senders: instance.allowed_senders,
                            max_body_bytes: instance.max_body_bytes,
                            max_attachment_bytes: instance.max_attachment_bytes,
                        }
                    })
                    .collect::<Vec<_>>();

                let imap_host = std::env::var("EMAIL_IMAP_HOST")
                    .ok()
                    .or_else(|| email.imap_host.as_deref().and_then(resolve_env_value));
                let imap_username = std::env::var("EMAIL_IMAP_USERNAME")
                    .ok()
                    .or_else(|| email.imap_username.as_deref().and_then(resolve_env_value));
                let imap_password = std::env::var("EMAIL_IMAP_PASSWORD")
                    .ok()
                    .or_else(|| email.imap_password.as_deref().and_then(resolve_env_value));
                let smtp_host = std::env::var("EMAIL_SMTP_HOST")
                    .ok()
                    .or_else(|| email.smtp_host.as_deref().and_then(resolve_env_value));

                let has_default = imap_host.is_some()
                    && imap_username.is_some()
                    && imap_password.is_some()
                    && smtp_host.is_some();

                if !has_default && instances.is_empty() {
                    return None;
                }

                let imap_host = imap_host.unwrap_or_default();
                let imap_username = imap_username.unwrap_or_default();
                let imap_password = imap_password.unwrap_or_default();
                let smtp_host = smtp_host.unwrap_or_default();
                let smtp_username = std::env::var("EMAIL_SMTP_USERNAME")
                    .ok()
                    .or_else(|| email.smtp_username.as_deref().and_then(resolve_env_value))
                    .unwrap_or_else(|| imap_username.clone());
                let smtp_password = std::env::var("EMAIL_SMTP_PASSWORD")
                    .ok()
                    .or_else(|| email.smtp_password.as_deref().and_then(resolve_env_value))
                    .unwrap_or_else(|| imap_password.clone());

                let from_address = std::env::var("EMAIL_FROM_ADDRESS")
                    .ok()
                    .or_else(|| email.from_address.as_deref().and_then(resolve_env_value))
                    .unwrap_or_else(|| smtp_username.clone());
                let from_name = std::env::var("EMAIL_FROM_NAME")
                    .ok()
                    .or_else(|| email.from_name.as_deref().and_then(resolve_env_value));

                Some(EmailConfig {
                    enabled: email.enabled,
                    imap_host,
                    imap_port: email.imap_port,
                    imap_username,
                    imap_password,
                    imap_use_tls: email.imap_use_tls,
                    smtp_host,
                    smtp_port: email.smtp_port,
                    smtp_username,
                    smtp_password,
                    smtp_use_starttls: email.smtp_use_starttls,
                    from_address,
                    from_name,
                    poll_interval_secs: email.poll_interval_secs,
                    folders: if email.folders.is_empty() {
                        vec!["INBOX".to_string()]
                    } else {
                        email.folders
                    },
                    allowed_senders: email.allowed_senders,
                    max_body_bytes: email.max_body_bytes,
                    max_attachment_bytes: email.max_attachment_bytes,
                    instances,
                })
            }),
            webhook: toml.messaging.webhook.map(|w| WebhookConfig {
                enabled: w.enabled,
                port: w.port,
                bind: w.bind,
                auth_token: w.auth_token.as_deref().and_then(resolve_env_value),
            }),
            twitch: toml.messaging.twitch.and_then(|t| {
                let instances = t
                    .instances
                    .into_iter()
                    .map(|instance| {
                        let username = instance.username.as_deref().and_then(resolve_env_value);
                        let oauth_token = instance
                            .oauth_token
                            .as_deref()
                            .and_then(resolve_env_value);
                        if instance.enabled && (username.is_none() || oauth_token.is_none()) {
                            tracing::warn!(
                                adapter = %instance.name,
                                "twitch instance is enabled but credentials are missing/unresolvable — disabling"
                            );
                        }
                        let has_credentials = username.is_some() && oauth_token.is_some();
                        let client_id = instance.client_id.as_deref().and_then(resolve_env_value);
                        let client_secret = instance
                            .client_secret
                            .as_deref()
                            .and_then(resolve_env_value);
                        let refresh_token = instance
                            .refresh_token
                            .as_deref()
                            .and_then(resolve_env_value);
                        TwitchInstanceConfig {
                            name: instance.name,
                            enabled: instance.enabled && has_credentials,
                            username: username.unwrap_or_default(),
                            oauth_token: oauth_token.unwrap_or_default(),
                            client_id,
                            client_secret,
                            refresh_token,
                            channels: instance.channels,
                            trigger_prefix: instance.trigger_prefix,
                        }
                    })
                    .collect::<Vec<_>>();

                let username = std::env::var("TWITCH_BOT_USERNAME")
                    .ok()
                    .or_else(|| t.username.as_deref().and_then(resolve_env_value));
                let oauth_token = std::env::var("TWITCH_OAUTH_TOKEN")
                    .ok()
                    .or_else(|| t.oauth_token.as_deref().and_then(resolve_env_value));

                if (username.is_none() || oauth_token.is_none()) && instances.is_empty() {
                    return None;
                }

                let client_id = t
                    .client_id
                    .as_deref()
                    .and_then(resolve_env_value)
                    .or_else(|| std::env::var("TWITCH_CLIENT_ID").ok());
                let client_secret = t
                    .client_secret
                    .as_deref()
                    .and_then(resolve_env_value)
                    .or_else(|| std::env::var("TWITCH_CLIENT_SECRET").ok());
                let refresh_token = t
                    .refresh_token
                    .as_deref()
                    .and_then(resolve_env_value)
                    .or_else(|| std::env::var("TWITCH_REFRESH_TOKEN").ok());
                Some(TwitchConfig {
                    enabled: t.enabled,
                    username: username.unwrap_or_default(),
                    oauth_token: oauth_token.unwrap_or_default(),
                    client_id,
                    client_secret,
                    refresh_token,
                    instances,
                    channels: t.channels,
                    trigger_prefix: t.trigger_prefix,
                })
            }),
        };

        let bindings: Vec<Binding> = toml
            .bindings
            .into_iter()
            .map(|b| Binding {
                agent_id: b.agent_id,
                channel: b.channel,
                adapter: normalize_adapter(b.adapter),
                guild_id: b.guild_id,
                workspace_id: b.workspace_id,
                chat_id: b.chat_id,
                channel_ids: b.channel_ids,
                require_mention: b.require_mention,
                dm_allowed_users: b.dm_allowed_users,
            })
            .collect();

        validate_named_messaging_adapters(&messaging, &bindings)?;

        let api = ApiConfig {
            enabled: toml.api.enabled,
            port: toml.api.port,
            bind: hosted_api_bind(toml.api.bind),
            auth_token: toml.api.auth_token.as_deref().and_then(resolve_env_value),
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

        let links = toml
            .links
            .into_iter()
            .map(|l| {
                // Backward compat: use `relationship` field if `kind` is default and `relationship` is set
                let kind = if l.kind == "peer" {
                    l.relationship.unwrap_or(l.kind)
                } else {
                    l.kind
                };
                LinkDef {
                    from: l.from,
                    to: l.to,
                    direction: l.direction,
                    kind,
                }
            })
            .collect();

        let groups = toml
            .groups
            .into_iter()
            .map(|g| GroupDef {
                name: g.name,
                agent_ids: g.agent_ids,
                color: g.color,
            })
            .collect();

        let mut humans: Vec<HumanDef> = toml
            .humans
            .into_iter()
            .map(|h| HumanDef {
                id: h.id,
                display_name: h.display_name,
                role: h.role,
                bio: h.bio,
            })
            .collect();

        // Default admin human if none defined
        if humans.is_empty() {
            humans.push(HumanDef {
                id: "admin".into(),
                display_name: None,
                role: None,
                bio: None,
            });
        }

        Ok(Config {
            instance_dir,
            llm,
            defaults,
            agents,
            links,
            groups,
            humans,
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
    pub cron_timezone: ArcSwap<Option<String>>,
    pub user_timezone: ArcSwap<Option<String>>,
    pub cortex: ArcSwap<CortexConfig>,
    pub warmup: ArcSwap<WarmupConfig>,
    /// Current warmup lifecycle status for API and observability.
    pub warmup_status: ArcSwap<WarmupStatus>,
    /// Synchronizes warmup passes so periodic and API-triggered runs don't overlap.
    pub warmup_lock: Arc<tokio::sync::Mutex<()>>,
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
    /// Sandbox configuration for process containment.
    pub sandbox: ArcSwap<crate::sandbox::SandboxConfig>,
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
            cron_timezone: ArcSwap::from_pointee(agent_config.cron_timezone.clone()),
            user_timezone: ArcSwap::from_pointee(agent_config.user_timezone.clone()),
            cortex: ArcSwap::from_pointee(agent_config.cortex),
            warmup: ArcSwap::from_pointee(agent_config.warmup),
            warmup_status: ArcSwap::from_pointee(WarmupStatus::default()),
            warmup_lock: Arc::new(tokio::sync::Mutex::new(())),
            memory_bulletin: ArcSwap::from_pointee(String::new()),
            prompts: ArcSwap::from_pointee(prompts),
            identity: ArcSwap::from_pointee(identity),
            skills: ArcSwap::from_pointee(skills),
            opencode: ArcSwap::from_pointee(defaults.opencode.clone()),
            opencode_server_pool: Arc::new(server_pool),
            cron_store: ArcSwap::from_pointee(None),
            cron_scheduler: ArcSwap::from_pointee(None),
            settings: ArcSwap::from_pointee(None),
            sandbox: ArcSwap::from_pointee(agent_config.sandbox.clone()),
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

    /// Compute the current dispatch-readiness signal.
    pub fn work_readiness(&self) -> WorkReadiness {
        let warmup_config = **self.warmup.load();
        let status = self.warmup_status.load().as_ref().clone();
        evaluate_work_readiness(warmup_config, status, chrono::Utc::now().timestamp_millis())
    }

    /// True when branch/worker/cron dispatches should run in fully-ready mode.
    pub fn ready_for_work(&self) -> bool {
        self.work_readiness().ready
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
        self.cron_timezone.store(Arc::new(resolved.cron_timezone));
        self.user_timezone.store(Arc::new(resolved.user_timezone));
        self.cortex.store(Arc::new(resolved.cortex));
        self.warmup.store(Arc::new(resolved.warmup));
        // sandbox config is not hot-reloaded here because the Sandbox instance
        // is constructed once at startup and shared via Arc. Changing sandbox
        // settings requires an agent restart.

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
#[allow(clippy::too_many_arguments)]
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
    agent_links: Arc<arc_swap::ArcSwap<Vec<crate::links::AgentLink>>>,
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
        if instance_skills_dir.is_dir()
            && let Err(error) = watcher.watch(&instance_skills_dir, RecursiveMode::Recursive)
        {
            tracing::warn!(%error, path = %instance_skills_dir.display(), "failed to watch instance skills dir");
        }

        // Watch per-agent workspace directories (skills, identity)
        for (_, workspace, _, _) in &agents {
            {
                let path = workspace.join("skills");
                if path.is_dir()
                    && let Err(error) = watcher.watch(&path, RecursiveMode::Recursive)
                {
                    tracing::warn!(%error, path = %path.display(), "failed to watch agent dir");
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

        while let Ok(first) = rx.recv() {
            // Drain any additional events within the debounce window
            let mut changed_paths: Vec<PathBuf> = first.paths;
            while let Ok(event) = rx.recv_timeout(debounce) {
                changed_paths.extend(event.paths);
            }

            // Categorize what changed
            let mut config_changed = changed_paths.iter().any(|p| p.ends_with("config.toml"));
            let identity_changed = changed_paths.iter().any(|p| {
                let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
                matches!(name, "SOUL.md" | "IDENTITY.md" | "USER.md" | "ROLE.md")
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

                match crate::links::AgentLink::from_config(&config.links) {
                    Ok(links) => {
                        agent_links.store(Arc::new(links));
                        tracing::info!("agent links reloaded ({} entries)", config.links.len());
                    }
                    Err(error) => {
                        tracing::error!(%error, "failed to parse links from reloaded config");
                    }
                }

                if let Some(ref perms) = discord_permissions
                    && let Some(discord_config) = &config.messaging.discord
                {
                    let new_perms =
                        DiscordPermissions::from_config(discord_config, &config.bindings);
                    perms.store(Arc::new(new_perms));
                    tracing::info!("discord permissions reloaded");
                }

                if let Some(ref perms) = slack_permissions
                    && let Some(slack_config) = &config.messaging.slack
                {
                    let new_perms = SlackPermissions::from_config(slack_config, &config.bindings);
                    perms.store(Arc::new(new_perms));
                    tracing::info!("slack permissions reloaded");
                }

                if let Some(ref perms) = telegram_permissions
                    && let Some(telegram_config) = &config.messaging.telegram
                {
                    let new_perms =
                        TelegramPermissions::from_config(telegram_config, &config.bindings);
                    perms.store(Arc::new(new_perms));
                    tracing::info!("telegram permissions reloaded");
                }

                if let Some(ref perms) = twitch_permissions
                    && let Some(twitch_config) = &config.messaging.twitch
                {
                    let new_perms = TwitchPermissions::from_config(twitch_config, &config.bindings);
                    perms.store(Arc::new(new_perms));
                    tracing::info!("twitch permissions reloaded");
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
                    let instance_dir = instance_dir.clone();

                    rt.spawn(async move {
                        // Discord: start default + named instances that are enabled and not already running.
                        if let Some(discord_config) = &config.messaging.discord
                            && discord_config.enabled {
                                if !discord_config.token.is_empty() && !manager.has_adapter("discord").await {
                                    let permissions = match discord_permissions {
                                        Some(ref existing) => existing.clone(),
                                        None => {
                                            let permissions = DiscordPermissions::from_config(discord_config, &config.bindings);
                                            Arc::new(arc_swap::ArcSwap::from_pointee(permissions))
                                        }
                                    };
                                    let adapter = crate::messaging::discord::DiscordAdapter::new(
                                        "discord",
                                        &discord_config.token,
                                        permissions,
                                    );
                                    if let Err(error) = manager.register_and_start(adapter).await {
                                        tracing::error!(%error, "failed to hot-start discord adapter from config change");
                                    }
                                }

                                for instance in discord_config.instances.iter().filter(|instance| instance.enabled) {
                                    let runtime_key = binding_runtime_adapter_key(
                                        "discord",
                                        Some(instance.name.as_str()),
                                    );
                                    if manager.has_adapter(runtime_key.as_str()).await {
                                        // TODO: named instance permissions are not hot-updated on
                                        // config reload because each instance owns its own
                                        // Arc<ArcSwap> with no external handle. Fixing this
                                        // requires either a permission-update method on the
                                        // Messaging trait or a shared handle registry. Permissions
                                        // will be correct after a full restart.
                                        continue;
                                    }

                                    let permissions = Arc::new(arc_swap::ArcSwap::from_pointee(
                                        DiscordPermissions::from_instance_config(instance, &config.bindings),
                                    ));
                                    let adapter = crate::messaging::discord::DiscordAdapter::new(
                                        runtime_key,
                                        &instance.token,
                                        permissions,
                                    );
                                    if let Err(error) = manager.register_and_start(adapter).await {
                                        tracing::error!(%error, adapter = %instance.name, "failed to hot-start named discord adapter from config change");
                                    }
                                }
                            }

                        // Slack: start default + named instances that are enabled and not already running.
                        if let Some(slack_config) = &config.messaging.slack
                            && slack_config.enabled {
                                if !slack_config.bot_token.is_empty()
                                    && !slack_config.app_token.is_empty()
                                    && !manager.has_adapter("slack").await
                                {
                                    let permissions = match slack_permissions {
                                        Some(ref existing) => existing.clone(),
                                        None => {
                                            let permissions = SlackPermissions::from_config(slack_config, &config.bindings);
                                            Arc::new(arc_swap::ArcSwap::from_pointee(permissions))
                                        }
                                    };
                                    match crate::messaging::slack::SlackAdapter::new(
                                        "slack",
                                        &slack_config.bot_token,
                                        &slack_config.app_token,
                                        permissions,
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

                                for instance in slack_config.instances.iter().filter(|instance| instance.enabled) {
                                    let runtime_key = binding_runtime_adapter_key(
                                        "slack",
                                        Some(instance.name.as_str()),
                                    );
                                    if manager.has_adapter(runtime_key.as_str()).await {
                                        // TODO: named instance permissions not hot-updated (see discord block comment)
                                        continue;
                                    }

                                    let permissions = Arc::new(arc_swap::ArcSwap::from_pointee(
                                        SlackPermissions::from_instance_config(instance, &config.bindings),
                                    ));
                                    match crate::messaging::slack::SlackAdapter::new(
                                        runtime_key,
                                        &instance.bot_token,
                                        &instance.app_token,
                                        permissions,
                                        instance.commands.clone(),
                                    ) {
                                        Ok(adapter) => {
                                            if let Err(error) = manager.register_and_start(adapter).await {
                                                tracing::error!(%error, adapter = %instance.name, "failed to hot-start named slack adapter from config change");
                                            }
                                        }
                                        Err(error) => {
                                            tracing::error!(%error, adapter = %instance.name, "failed to build named slack adapter from config change");
                                        }
                                    }
                                }
                            }

                        // Telegram: start default + named instances that are enabled and not already running.
                        if let Some(telegram_config) = &config.messaging.telegram
                            && telegram_config.enabled {
                                if !telegram_config.token.is_empty()
                                    && !manager.has_adapter("telegram").await
                                {
                                    let permissions = match telegram_permissions {
                                        Some(ref existing) => existing.clone(),
                                        None => {
                                            let permissions = TelegramPermissions::from_config(telegram_config, &config.bindings);
                                            Arc::new(arc_swap::ArcSwap::from_pointee(permissions))
                                        }
                                    };
                                    let adapter = crate::messaging::telegram::TelegramAdapter::new(
                                        "telegram",
                                        &telegram_config.token,
                                        permissions,
                                    );
                                    if let Err(error) = manager.register_and_start(adapter).await {
                                        tracing::error!(%error, "failed to hot-start telegram adapter from config change");
                                    }
                                }

                                for instance in telegram_config.instances.iter().filter(|instance| instance.enabled) {
                                    let runtime_key = binding_runtime_adapter_key(
                                        "telegram",
                                        Some(instance.name.as_str()),
                                    );
                                    if manager.has_adapter(runtime_key.as_str()).await {
                                        // TODO: named instance permissions not hot-updated (see discord block comment)
                                        continue;
                                    }

                                    let permissions = Arc::new(arc_swap::ArcSwap::from_pointee(
                                        TelegramPermissions::from_instance_config(instance, &config.bindings),
                                    ));
                                    let adapter = crate::messaging::telegram::TelegramAdapter::new(
                                        runtime_key,
                                        &instance.token,
                                        permissions,
                                    );
                                    if let Err(error) = manager.register_and_start(adapter).await {
                                        tracing::error!(%error, adapter = %instance.name, "failed to hot-start named telegram adapter from config change");
                                    }
                                }
                            }

                        // Email: start default + named instances that are enabled and not already running.
                        if let Some(email_config) = &config.messaging.email
                            && email_config.enabled {
                                if !email_config.imap_host.is_empty() && !manager.has_adapter("email").await {
                                    match crate::messaging::email::EmailAdapter::from_config(email_config) {
                                        Ok(adapter) => {
                                            if let Err(error) = manager.register_and_start(adapter).await {
                                                tracing::error!(%error, "failed to hot-start email adapter from config change");
                                            }
                                        }
                                        Err(error) => {
                                            tracing::error!(%error, "failed to build email adapter from config change");
                                        }
                                    }
                                }

                                for instance in email_config.instances.iter().filter(|instance| instance.enabled) {
                                    let runtime_key = binding_runtime_adapter_key(
                                        "email",
                                        Some(instance.name.as_str()),
                                    );
                                    if manager.has_adapter(runtime_key.as_str()).await {
                                        continue;
                                    }

                                    match crate::messaging::email::EmailAdapter::from_instance_config(
                                        runtime_key.as_str(),
                                        instance,
                                    ) {
                                        Ok(adapter) => {
                                            if let Err(error) = manager.register_and_start(adapter).await {
                                                tracing::error!(%error, adapter = %instance.name, "failed to hot-start named email adapter from config change");
                                            }
                                        }
                                        Err(error) => {
                                            tracing::error!(%error, adapter = %instance.name, "failed to build named email adapter from config change");
                                        }
                                    }
                                }
                            }

                        // Twitch: start default + named instances that are enabled and not already running.
                        if let Some(twitch_config) = &config.messaging.twitch
                            && twitch_config.enabled {
                                if !twitch_config.username.is_empty()
                                    && !twitch_config.oauth_token.is_empty()
                                    && !manager.has_adapter("twitch").await
                                {
                                    let permissions = match twitch_permissions {
                                        Some(ref existing) => existing.clone(),
                                        None => {
                                            let permissions = TwitchPermissions::from_config(twitch_config, &config.bindings);
                                            Arc::new(arc_swap::ArcSwap::from_pointee(permissions))
                                        }
                                    };
                                    let token_path = instance_dir.join("twitch_token.json");
                                    let adapter = crate::messaging::twitch::TwitchAdapter::new(
                                        "twitch",
                                        &twitch_config.username,
                                        &twitch_config.oauth_token,
                                        twitch_config.client_id.clone(),
                                        twitch_config.client_secret.clone(),
                                        twitch_config.refresh_token.clone(),
                                        Some(token_path),
                                        twitch_config.channels.clone(),
                                        twitch_config.trigger_prefix.clone(),
                                        permissions,
                                    );
                                    if let Err(error) = manager.register_and_start(adapter).await {
                                        tracing::error!(%error, "failed to hot-start twitch adapter from config change");
                                    }
                                }

                                for instance in twitch_config.instances.iter().filter(|instance| instance.enabled) {
                                    let runtime_key = binding_runtime_adapter_key(
                                        "twitch",
                                        Some(instance.name.as_str()),
                                    );
                                    if manager.has_adapter(runtime_key.as_str()).await {
                                        // TODO: named instance permissions not hot-updated (see discord block comment)
                                        continue;
                                    }

                                    let token_file_name = {
                                        use std::hash::{Hash, Hasher};
                                        let mut hasher = std::collections::hash_map::DefaultHasher::new();
                                        instance.name.hash(&mut hasher);
                                        let name_hash = hasher.finish();
                                        format!(
                                            "twitch_token_{}_{name_hash:016x}.json",
                                            instance
                                                .name
                                                .chars()
                                                .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '_' })
                                                .collect::<String>()
                                        )
                                    };
                                    let token_path = instance_dir.join(token_file_name);
                                    let permissions = Arc::new(arc_swap::ArcSwap::from_pointee(
                                        TwitchPermissions::from_instance_config(instance, &config.bindings),
                                    ));
                                    let adapter = crate::messaging::twitch::TwitchAdapter::new(
                                        runtime_key,
                                        &instance.username,
                                        &instance.oauth_token,
                                        instance.client_id.clone(),
                                        instance.client_secret.clone(),
                                        instance.refresh_token.clone(),
                                        Some(token_path),
                                        instance.channels.clone(),
                                        instance.trigger_prefix.clone(),
                                        permissions,
                                    );
                                    if let Err(error) = manager.register_and_start(adapter).await {
                                        tracing::error!(%error, adapter = %instance.name, "failed to hot-start named twitch adapter from config change");
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
        "Gemini",
        "Ollama",
        "OpenCode Zen",
        "OpenCode Go",
        "MiniMax",
        "Moonshot AI (Kimi)",
        "Z.AI Coding Plan",
        "Kilo Gateway",
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
        10 => ("Google Gemini API key", "gemini_key", "gemini"),
        11 => ("Ollama base URL (optional)", "ollama_base_url", "ollama"),
        12 => ("OpenCode Zen API key", "opencode_zen_key", "opencode-zen"),
        13 => ("OpenCode Go API key", "opencode_go_key", "opencode-go"),
        14 => ("MiniMax API key", "minimax_key", "minimax"),
        15 => ("Moonshot API key", "moonshot_key", "moonshot"),
        16 => (
            "Z.AI Coding Plan API key",
            "zai_coding_plan_key",
            "zai-coding-plan",
        ),
        17 => ("Kilo Gateway API key", "kilo_key", "kilo"),
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
            const KEYS: [&str; 26] = [
                "SPACEBOT_DIR",
                "SPACEBOT_DEPLOYMENT",
                "SPACEBOT_CRON_TIMEZONE",
                "SPACEBOT_USER_TIMEZONE",
                "ANTHROPIC_API_KEY",
                "ANTHROPIC_OAUTH_TOKEN",
                "OPENAI_API_KEY",
                "OPENROUTER_API_KEY",
                "KILO_API_KEY",
                "ZHIPU_API_KEY",
                "GROQ_API_KEY",
                "TOGETHER_API_KEY",
                "FIREWORKS_API_KEY",
                "DEEPSEEK_API_KEY",
                "XAI_API_KEY",
                "MISTRAL_API_KEY",
                "GEMINI_API_KEY",
                "NVIDIA_API_KEY",
                "OLLAMA_API_KEY",
                "OLLAMA_BASE_URL",
                "OPENCODE_ZEN_API_KEY",
                "OPENCODE_GO_API_KEY",
                "MINIMAX_API_KEY",
                "MINIMAX_CN_API_KEY",
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
api_type = "openai_chat_completions"
base_url = "https://api.example.com"
api_key = "test-key"
"#;
        let result2: StdResult<TomlProviderConfig, toml::de::Error> = toml::from_str(toml2);
        assert!(result2.is_ok(), "Error: {:?}", result2.err());
        assert_eq!(result2.unwrap().api_type, ApiType::OpenAiChatCompletions);

        let toml3 = r#"
api_type = "kilo_gateway"
base_url = "https://api.kilo.ai/api/gateway"
api_key = "test-key"
"#;
        let result3: StdResult<TomlProviderConfig, toml::de::Error> = toml::from_str(toml3);
        assert!(result3.is_ok(), "Error: {:?}", result3.err());
        assert_eq!(result3.unwrap().api_type, ApiType::KiloGateway);

        let toml4 = r#"
api_type = "openai_responses"
base_url = "https://api.openai.com"
api_key = "test-key"
"#;
        let result4: StdResult<TomlProviderConfig, toml::de::Error> = toml::from_str(toml4);
        assert!(result4.is_ok(), "Error: {:?}", result4.err());
        assert_eq!(result4.unwrap().api_type, ApiType::OpenAiResponses);

        let toml5 = r#"
api_type = "anthropic"
base_url = "https://api.anthropic.com"
api_key = "test-key"
"#;
        let result5: StdResult<TomlProviderConfig, toml::de::Error> = toml::from_str(toml5);
        assert!(result5.is_ok(), "Error: {:?}", result5.err());
        assert_eq!(result5.unwrap().api_type, ApiType::Anthropic);
    }

    #[test]
    fn test_api_type_deserialization_invalid() {
        let toml = r#"api_type = "invalid_type""#;
        let result: StdResult<TomlProviderConfig, toml::de::Error> = toml::from_str(toml);
        assert!(result.is_err());
        let error = result.unwrap_err();
        assert!(error.to_string().contains("invalid value"));
        assert!(error.to_string().contains("openai_completions"));
        assert!(error.to_string().contains("openai_chat_completions"));
        assert!(error.to_string().contains("kilo_gateway"));
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
        let _lock = env_test_lock().lock().unwrap_or_else(|e| e.into_inner());
        let _env = EnvGuard::new();

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
            expires_at: chrono::Utc::now().timestamp_millis() + 3_600_000,
        };
        crate::auth::save_credentials(&instance_dir, &creds).expect("failed to save credentials");

        assert!(!Config::needs_onboarding());
    }

    #[test]
    fn test_needs_onboarding_false_with_openai_oauth_credentials() {
        let _lock = env_test_lock()
            .lock()
            .expect("failed to lock env test mutex");
        let _env = EnvGuard::new();

        let instance_dir = Config::default_instance_dir();
        let creds = crate::openai_auth::OAuthCredentials {
            access_token: "openai-access-token-test".to_string(),
            refresh_token: "openai-refresh-token-test".to_string(),
            expires_at: chrono::Utc::now().timestamp_millis() + 3_600_000,
            account_id: Some("acct_test_123".to_string()),
        };
        crate::openai_auth::save_credentials(&instance_dir, &creds)
            .expect("failed to save OpenAI OAuth credentials");

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

    /// Helper to build a minimal `SlackConfig` for permission tests.
    fn slack_config_with_dm_users(dm_allowed_users: Vec<String>) -> SlackConfig {
        SlackConfig {
            enabled: true,
            bot_token: "xoxb-test".into(),
            app_token: "xapp-test".into(),
            instances: vec![],
            dm_allowed_users,
            commands: vec![],
        }
    }

    /// Helper to build a Slack binding with optional dm_allowed_users.
    fn slack_binding(workspace_id: Option<&str>, dm_allowed_users: Vec<String>) -> Binding {
        Binding {
            agent_id: "test-agent".into(),
            channel: "slack".into(),
            adapter: None,
            guild_id: None,
            workspace_id: workspace_id.map(String::from),
            chat_id: None,
            channel_ids: vec![],
            require_mention: false,
            dm_allowed_users,
        }
    }

    #[test]
    fn slack_permissions_merges_dm_users_from_config_and_bindings() {
        let config = slack_config_with_dm_users(vec!["U001".into(), "U002".into()]);
        let bindings = vec![slack_binding(
            Some("T1"),
            vec!["U003".into(), "U004".into()],
        )];
        let perms = SlackPermissions::from_config(&config, &bindings);
        assert_eq!(perms.dm_allowed_users, vec!["U001", "U002", "U003", "U004"]);
    }

    #[test]
    fn slack_permissions_deduplicates_dm_users() {
        let config = slack_config_with_dm_users(vec!["U001".into(), "U002".into()]);
        let bindings = vec![slack_binding(
            Some("T1"),
            vec!["U002".into(), "U003".into()],
        )];
        let perms = SlackPermissions::from_config(&config, &bindings);
        // U002 appears in both config and binding — should appear only once
        assert_eq!(perms.dm_allowed_users, vec!["U001", "U002", "U003"]);
    }

    #[test]
    fn slack_permissions_empty_dm_users_stays_empty() {
        let config = slack_config_with_dm_users(vec![]);
        let bindings = vec![slack_binding(Some("T1"), vec![])];
        let perms = SlackPermissions::from_config(&config, &bindings);
        assert!(perms.dm_allowed_users.is_empty());
    }

    #[test]
    fn slack_permissions_merges_dm_users_from_multiple_bindings() {
        let config = slack_config_with_dm_users(vec!["U001".into()]);
        let bindings = vec![
            slack_binding(Some("T1"), vec!["U002".into()]),
            slack_binding(Some("T2"), vec!["U003".into()]),
        ];
        let perms = SlackPermissions::from_config(&config, &bindings);
        assert_eq!(perms.dm_allowed_users, vec!["U001", "U002", "U003"]);
    }

    #[test]
    fn slack_permissions_ignores_non_slack_bindings() {
        let config = slack_config_with_dm_users(vec!["U001".into()]);
        let mut discord_binding = slack_binding(Some("T1"), vec!["U099".into()]);
        discord_binding.channel = "discord".into();
        let perms = SlackPermissions::from_config(&config, &[discord_binding]);
        // U099 should not appear — that binding is for discord, not slack
        assert_eq!(perms.dm_allowed_users, vec!["U001"]);
    }

    #[test]
    fn slack_permissions_workspace_filter_from_bindings() {
        let config = slack_config_with_dm_users(vec![]);
        let bindings = vec![
            slack_binding(Some("T1"), vec![]),
            slack_binding(Some("T2"), vec![]),
        ];
        let perms = SlackPermissions::from_config(&config, &bindings);
        assert_eq!(
            perms.workspace_filter,
            Some(vec!["T1".to_string(), "T2".to_string()])
        );
    }

    #[test]
    fn slack_permissions_no_workspace_filter_when_none_specified() {
        let config = slack_config_with_dm_users(vec![]);
        let bindings = vec![slack_binding(None, vec![])];
        let perms = SlackPermissions::from_config(&config, &bindings);
        assert!(perms.workspace_filter.is_none());
    }

    #[test]
    fn test_cron_timezone_resolution_precedence() {
        let _lock = env_test_lock()
            .lock()
            .expect("failed to lock env test mutex");
        let _env = EnvGuard::new();

        unsafe {
            std::env::set_var(CRON_TIMEZONE_ENV_VAR, "Asia/Tokyo");
        }

        let toml = r#"
[defaults]
cron_timezone = "America/New_York"

[[agents]]
id = "main"
cron_timezone = "Europe/Berlin"
"#;

        let parsed: TomlConfig = toml::from_str(toml).expect("failed to parse test TOML");
        let config = Config::from_toml(parsed, PathBuf::from(".")).expect("failed to build Config");

        assert_eq!(
            config.defaults.cron_timezone.as_deref(),
            Some("America/New_York")
        );
        assert_eq!(
            config.agents[0].cron_timezone.as_deref(),
            Some("Europe/Berlin")
        );

        let resolved = config.agents[0].resolve(&config.instance_dir, &config.defaults);
        assert_eq!(resolved.cron_timezone.as_deref(), Some("Europe/Berlin"));

        let toml_without_agent_override = r#"
[defaults]
cron_timezone = "America/New_York"

[[agents]]
id = "main"
"#;
        let parsed: TomlConfig =
            toml::from_str(toml_without_agent_override).expect("failed to parse test TOML");
        let config = Config::from_toml(parsed, PathBuf::from(".")).expect("failed to build Config");
        let resolved = config.agents[0].resolve(&config.instance_dir, &config.defaults);
        assert_eq!(resolved.cron_timezone.as_deref(), Some("America/New_York"));

        let toml_without_default = r#"
[[agents]]
id = "main"
"#;
        let parsed: TomlConfig =
            toml::from_str(toml_without_default).expect("failed to parse test TOML");
        let config = Config::from_toml(parsed, PathBuf::from(".")).expect("failed to build Config");
        let resolved = config.agents[0].resolve(&config.instance_dir, &config.defaults);
        assert_eq!(resolved.cron_timezone.as_deref(), Some("Asia/Tokyo"));
    }

    #[test]
    fn test_cron_timezone_invalid_falls_back_to_system() {
        let _lock = env_test_lock()
            .lock()
            .expect("failed to lock env test mutex");
        let _env = EnvGuard::new();

        unsafe {
            std::env::set_var(CRON_TIMEZONE_ENV_VAR, "Not/A-Real-Tz");
        }

        let toml = r#"
[[agents]]
id = "main"
"#;

        let parsed: TomlConfig = toml::from_str(toml).expect("failed to parse test TOML");
        let config = Config::from_toml(parsed, PathBuf::from(".")).expect("failed to build Config");
        let resolved = config.agents[0].resolve(&config.instance_dir, &config.defaults);
        assert_eq!(resolved.cron_timezone, None);
    }

    #[test]
    fn test_cron_timezone_invalid_default_uses_env_fallback() {
        let _lock = env_test_lock()
            .lock()
            .expect("failed to lock env test mutex");
        let _env = EnvGuard::new();

        unsafe {
            std::env::set_var(CRON_TIMEZONE_ENV_VAR, "Asia/Tokyo");
        }

        let toml = r#"
[defaults]
cron_timezone = "Not/A-Real-Tz"

[[agents]]
id = "main"
"#;

        let parsed: TomlConfig = toml::from_str(toml).expect("failed to parse test TOML");
        let config = Config::from_toml(parsed, PathBuf::from(".")).expect("failed to build Config");
        let resolved = config.agents[0].resolve(&config.instance_dir, &config.defaults);
        assert_eq!(resolved.cron_timezone.as_deref(), Some("Asia/Tokyo"));
    }

    #[test]
    fn test_user_timezone_resolution_precedence() {
        let _lock = env_test_lock()
            .lock()
            .expect("failed to lock env test mutex");
        let _env = EnvGuard::new();

        unsafe {
            std::env::set_var(USER_TIMEZONE_ENV_VAR, "Asia/Tokyo");
        }

        let toml = r#"
[defaults]
user_timezone = "America/New_York"

[[agents]]
id = "main"
user_timezone = "Europe/Berlin"
"#;

        let parsed: TomlConfig = toml::from_str(toml).expect("failed to parse test TOML");
        let config = Config::from_toml(parsed, PathBuf::from(".")).expect("failed to build Config");
        let resolved = config.agents[0].resolve(&config.instance_dir, &config.defaults);
        assert_eq!(resolved.user_timezone.as_deref(), Some("Europe/Berlin"));

        let toml_without_agent_override = r#"
[defaults]
user_timezone = "America/New_York"

[[agents]]
id = "main"
"#;
        let parsed: TomlConfig =
            toml::from_str(toml_without_agent_override).expect("failed to parse test TOML");
        let config = Config::from_toml(parsed, PathBuf::from(".")).expect("failed to build Config");
        let resolved = config.agents[0].resolve(&config.instance_dir, &config.defaults);
        assert_eq!(resolved.user_timezone.as_deref(), Some("America/New_York"));

        let toml_without_default = r#"
[[agents]]
id = "main"
"#;
        let parsed: TomlConfig =
            toml::from_str(toml_without_default).expect("failed to parse test TOML");
        let config = Config::from_toml(parsed, PathBuf::from(".")).expect("failed to build Config");
        let resolved = config.agents[0].resolve(&config.instance_dir, &config.defaults);
        assert_eq!(resolved.user_timezone.as_deref(), Some("Asia/Tokyo"));
    }

    #[test]
    fn test_user_timezone_falls_back_to_cron_timezone() {
        let _lock = env_test_lock()
            .lock()
            .expect("failed to lock env test mutex");
        let _env = EnvGuard::new();

        let toml = r#"
[defaults]
cron_timezone = "America/Los_Angeles"

[[agents]]
id = "main"
"#;

        let parsed: TomlConfig = toml::from_str(toml).expect("failed to parse test TOML");
        let config = Config::from_toml(parsed, PathBuf::from(".")).expect("failed to build Config");
        let resolved = config.agents[0].resolve(&config.instance_dir, &config.defaults);
        assert_eq!(
            resolved.cron_timezone.as_deref(),
            Some("America/Los_Angeles")
        );
        assert_eq!(
            resolved.user_timezone.as_deref(),
            Some("America/Los_Angeles")
        );
    }

    #[test]
    fn test_user_timezone_invalid_falls_back_to_cron_timezone() {
        let _lock = env_test_lock()
            .lock()
            .expect("failed to lock env test mutex");
        let _env = EnvGuard::new();

        let toml = r#"
[defaults]
cron_timezone = "America/Los_Angeles"
user_timezone = "Not/A-Real-Tz"

[[agents]]
id = "main"
"#;

        let parsed: TomlConfig = toml::from_str(toml).expect("failed to parse test TOML");
        let config = Config::from_toml(parsed, PathBuf::from(".")).expect("failed to build Config");
        let resolved = config.agents[0].resolve(&config.instance_dir, &config.defaults);
        assert_eq!(
            resolved.user_timezone.as_deref(),
            Some("America/Los_Angeles")
        );
    }

    #[test]
    fn test_user_timezone_invalid_config_uses_env_fallback() {
        let _lock = env_test_lock()
            .lock()
            .expect("failed to lock env test mutex");
        let _env = EnvGuard::new();

        unsafe {
            std::env::set_var(USER_TIMEZONE_ENV_VAR, "Asia/Tokyo");
        }

        let toml = r#"
[defaults]
cron_timezone = "America/Los_Angeles"
user_timezone = "Not/A-Real-Tz"

[[agents]]
id = "main"
"#;

        let parsed: TomlConfig = toml::from_str(toml).expect("failed to parse test TOML");
        let config = Config::from_toml(parsed, PathBuf::from(".")).expect("failed to build Config");
        let resolved = config.agents[0].resolve(&config.instance_dir, &config.defaults);
        assert_eq!(resolved.user_timezone.as_deref(), Some("Asia/Tokyo"));
    }

    #[test]
    fn ollama_base_url_registers_provider() {
        let toml = r#"
[llm]
ollama_base_url = "http://localhost:11434"

[[agents]]
id = "main"
"#;
        let parsed: TomlConfig = toml::from_str(toml).expect("failed to parse test TOML");
        let config = Config::from_toml(parsed, PathBuf::from(".")).expect("failed to build Config");
        let provider = config
            .llm
            .providers
            .get("ollama")
            .expect("ollama provider should be registered");
        assert_eq!(provider.base_url, "http://localhost:11434");
        assert_eq!(provider.api_type, ApiType::OpenAiCompletions);
        assert_eq!(provider.api_key, "");
    }

    #[test]
    fn ollama_key_alone_registers_provider_with_default_url() {
        let toml = r#"
[llm]
ollama_key = "test-key"

[[agents]]
id = "main"
"#;
        let parsed: TomlConfig = toml::from_str(toml).expect("failed to parse test TOML");
        let config = Config::from_toml(parsed, PathBuf::from(".")).expect("failed to build Config");
        let provider = config
            .llm
            .providers
            .get("ollama")
            .expect("ollama provider should be registered");
        assert_eq!(provider.base_url, "http://localhost:11434");
        assert_eq!(provider.api_key, "test-key");
    }

    #[test]
    fn ollama_custom_provider_takes_precedence_over_shorthand() {
        // Custom provider block should win over shorthand keys (or_insert_with semantics)
        let toml = r#"
[llm]
ollama_base_url = "http://localhost:11434"

[llm.providers.ollama]
api_type = "openai_completions"
base_url = "http://remote-ollama:11434"
api_key = ""

[[agents]]
id = "main"
"#;
        let parsed: TomlConfig = toml::from_str(toml).expect("failed to parse test TOML");
        let config = Config::from_toml(parsed, PathBuf::from(".")).expect("failed to build Config");
        let provider = config
            .llm
            .providers
            .get("ollama")
            .expect("ollama provider should be registered");
        assert_eq!(provider.base_url, "http://remote-ollama:11434");
    }

    #[test]
    fn default_provider_config_ollama_uses_base_url_and_empty_api_key() {
        let provider = default_provider_config("ollama", "http://remote-ollama.local:11434")
            .expect("ollama provider should be supported");
        assert_eq!(provider.api_type, ApiType::OpenAiCompletions);
        assert_eq!(provider.base_url, "http://remote-ollama.local:11434");
        assert_eq!(provider.api_key, "");
    }

    #[test]
    fn test_warmup_defaults_applied_when_not_configured() {
        let toml = r#"
[[agents]]
id = "main"
"#;
        let parsed: TomlConfig = toml::from_str(toml).expect("failed to parse test TOML");
        let config = Config::from_toml(parsed, PathBuf::from(".")).expect("failed to build Config");
        let resolved = config.agents[0].resolve(&config.instance_dir, &config.defaults);

        assert!(config.defaults.warmup.enabled);
        assert!(config.defaults.warmup.eager_embedding_load);
        assert_eq!(config.defaults.warmup.refresh_secs, 900);
        assert_eq!(config.defaults.warmup.startup_delay_secs, 5);

        assert_eq!(resolved.warmup.enabled, config.defaults.warmup.enabled);
        assert_eq!(
            resolved.warmup.eager_embedding_load,
            config.defaults.warmup.eager_embedding_load
        );
        assert_eq!(
            resolved.warmup.refresh_secs,
            config.defaults.warmup.refresh_secs
        );
        assert_eq!(
            resolved.warmup.startup_delay_secs,
            config.defaults.warmup.startup_delay_secs
        );
    }

    #[test]
    fn test_warmup_default_and_agent_override_resolution() {
        let toml = r#"
[defaults.warmup]
enabled = false
eager_embedding_load = false
refresh_secs = 120
startup_delay_secs = 9

[[agents]]
id = "main"

[agents.warmup]
enabled = true
startup_delay_secs = 2
"#;
        let parsed: TomlConfig = toml::from_str(toml).expect("failed to parse test TOML");
        let config = Config::from_toml(parsed, PathBuf::from(".")).expect("failed to build Config");
        let resolved = config.agents[0].resolve(&config.instance_dir, &config.defaults);

        assert!(!config.defaults.warmup.enabled);
        assert!(!config.defaults.warmup.eager_embedding_load);
        assert_eq!(config.defaults.warmup.refresh_secs, 120);
        assert_eq!(config.defaults.warmup.startup_delay_secs, 9);

        assert!(resolved.warmup.enabled);
        assert!(!resolved.warmup.eager_embedding_load);
        assert_eq!(resolved.warmup.refresh_secs, 120);
        assert_eq!(resolved.warmup.startup_delay_secs, 2);
    }

    #[test]
    fn test_work_readiness_requires_warm_state() {
        let readiness = evaluate_work_readiness(
            WarmupConfig::default(),
            WarmupStatus {
                state: WarmupState::Cold,
                embedding_ready: true,
                last_refresh_unix_ms: Some(1_000),
                last_error: None,
                bulletin_age_secs: None,
            },
            2_000,
        );

        assert!(!readiness.ready);
        assert_eq!(readiness.reason, Some(WorkReadinessReason::StateNotWarm));
    }

    #[test]
    fn test_work_readiness_requires_embedding_ready() {
        let readiness = evaluate_work_readiness(
            WarmupConfig::default(),
            WarmupStatus {
                state: WarmupState::Warm,
                embedding_ready: false,
                last_refresh_unix_ms: Some(1_000),
                last_error: None,
                bulletin_age_secs: None,
            },
            2_000,
        );

        assert!(!readiness.ready);
        assert_eq!(
            readiness.reason,
            Some(WorkReadinessReason::EmbeddingNotReady)
        );
    }

    #[test]
    fn test_work_readiness_does_not_require_embedding_when_eager_load_disabled() {
        let readiness = evaluate_work_readiness(
            WarmupConfig {
                eager_embedding_load: false,
                ..Default::default()
            },
            WarmupStatus {
                state: WarmupState::Warm,
                embedding_ready: false,
                last_refresh_unix_ms: Some(1_000),
                last_error: None,
                bulletin_age_secs: None,
            },
            2_000,
        );

        assert!(readiness.ready);
        assert_eq!(readiness.reason, None);
    }

    #[test]
    fn test_work_readiness_requires_bulletin_timestamp() {
        let readiness = evaluate_work_readiness(
            WarmupConfig::default(),
            WarmupStatus {
                state: WarmupState::Warm,
                embedding_ready: true,
                last_refresh_unix_ms: None,
                last_error: None,
                bulletin_age_secs: None,
            },
            2_000,
        );

        assert!(!readiness.ready);
        assert_eq!(readiness.reason, Some(WorkReadinessReason::BulletinMissing));
    }

    #[test]
    fn test_work_readiness_rejects_stale_bulletin() {
        let readiness = evaluate_work_readiness(
            WarmupConfig {
                refresh_secs: 60,
                ..Default::default()
            },
            WarmupStatus {
                state: WarmupState::Warm,
                embedding_ready: true,
                last_refresh_unix_ms: Some(1_000),
                last_error: None,
                bulletin_age_secs: None,
            },
            122_000,
        );

        assert_eq!(readiness.stale_after_secs, 120);
        assert_eq!(readiness.bulletin_age_secs, Some(121));
        assert!(!readiness.ready);
        assert_eq!(readiness.reason, Some(WorkReadinessReason::BulletinStale));
    }

    #[test]
    fn test_work_readiness_ready_when_all_constraints_hold() {
        let readiness = evaluate_work_readiness(
            WarmupConfig {
                refresh_secs: 120,
                ..Default::default()
            },
            WarmupStatus {
                state: WarmupState::Warm,
                embedding_ready: true,
                last_refresh_unix_ms: Some(200_000),
                last_error: None,
                bulletin_age_secs: None,
            },
            310_000,
        );

        assert!(readiness.ready);
        assert_eq!(readiness.reason, None);
        assert_eq!(readiness.bulletin_age_secs, Some(110));
    }

    /// Verify that every shorthand key field in `LlmConfig` actually registers a provider.
    ///
    /// This is a regression test for the recurring "unknown provider: X" bug pattern
    /// (nvidia #82, ollama #175, deepseek #179). If a new shorthand key is added to
    /// `LlmConfig` without wiring it up in `load_from_env` / `from_toml`, this test fails.
    #[test]
    fn all_shorthand_keys_register_providers_via_toml() {
        // (toml_key, toml_value, provider_name, expected_base_url_substring)
        let cases: &[(&str, &str, &str, &str)] = &[
            ("anthropic_key", "test-key", "anthropic", "anthropic.com"),
            ("openai_key", "test-key", "openai", "openai.com"),
            ("openrouter_key", "test-key", "openrouter", "openrouter.ai"),
            ("kilo_key", "test-key", "kilo", "api.kilo.ai"),
            ("deepseek_key", "test-key", "deepseek", "deepseek.com"),
            ("minimax_key", "test-key", "minimax", "minimax.io"),
            ("minimax_cn_key", "test-key", "minimax-cn", "minimaxi.com"),
            ("moonshot_key", "test-key", "moonshot", "moonshot.ai"),
            ("nvidia_key", "test-key", "nvidia", "nvidia.com"),
            ("fireworks_key", "test-key", "fireworks", "fireworks.ai"),
            ("zhipu_key", "test-key", "zhipu", "z.ai"),
            ("gemini_key", "test-key", "gemini", "google"),
            ("groq_key", "test-key", "groq", "groq.com"),
            ("together_key", "test-key", "together", "together"),
            ("xai_key", "test-key", "xai", "x.ai"),
            ("mistral_key", "test-key", "mistral", "mistral.ai"),
            (
                "opencode_zen_key",
                "test-key",
                "opencode-zen",
                "opencode.ai/zen",
            ),
            (
                "opencode_go_key",
                "test-key",
                "opencode-go",
                "opencode.ai/zen/go",
            ),
            (
                "ollama_base_url",
                "http://localhost:11434",
                "ollama",
                "localhost:11434",
            ),
        ];

        for (toml_key, toml_value, provider_name, url_substr) in cases {
            let toml_str =
                format!("[llm]\n{toml_key} = \"{toml_value}\"\n\n[[agents]]\nid = \"main\"\n");

            let parsed: TomlConfig = toml::from_str(&toml_str)
                .unwrap_or_else(|e| panic!("failed to parse toml for {toml_key}: {e}"));
            let config = Config::from_toml(parsed, PathBuf::from("."))
                .unwrap_or_else(|e| panic!("failed to build config for {toml_key}: {e}"));

            let provider = config.llm.providers.get(*provider_name).unwrap_or_else(|| {
                panic!(
                    "provider '{provider_name}' not registered when '{toml_key}' is set — \
                     add an .entry(\"{provider_name}\").or_insert_with(...) block in from_toml()"
                )
            });

            assert!(
                provider.base_url.contains(url_substr),
                "provider '{provider_name}' base_url '{}' does not contain '{url_substr}'",
                provider.base_url
            );
        }
    }

    #[test]
    fn all_shorthand_keys_register_providers_via_env() {
        let _lock = env_test_lock().lock().unwrap();

        // (env_var, env_value, provider_name, expected_base_url_substring)
        let cases: &[(&str, &str, &str, &str)] = &[
            (
                "ANTHROPIC_API_KEY",
                "test-key",
                "anthropic",
                "anthropic.com",
            ),
            ("OPENAI_API_KEY", "test-key", "openai", "openai.com"),
            (
                "OPENROUTER_API_KEY",
                "test-key",
                "openrouter",
                "openrouter.ai",
            ),
            ("KILO_API_KEY", "test-key", "kilo", "api.kilo.ai"),
            ("DEEPSEEK_API_KEY", "test-key", "deepseek", "deepseek.com"),
            ("MINIMAX_API_KEY", "test-key", "minimax", "minimax.io"),
            ("NVIDIA_API_KEY", "test-key", "nvidia", "nvidia.com"),
            ("FIREWORKS_API_KEY", "test-key", "fireworks", "fireworks.ai"),
            ("ZHIPU_API_KEY", "test-key", "zhipu", "z.ai"),
            ("GEMINI_API_KEY", "test-key", "gemini", "google"),
            ("GROQ_API_KEY", "test-key", "groq", "groq.com"),
            ("TOGETHER_API_KEY", "test-key", "together", "together"),
            ("XAI_API_KEY", "test-key", "xai", "x.ai"),
            ("MISTRAL_API_KEY", "test-key", "mistral", "mistral.ai"),
            (
                "OPENCODE_ZEN_API_KEY",
                "test-key",
                "opencode-zen",
                "opencode.ai/zen",
            ),
            (
                "OPENCODE_GO_API_KEY",
                "test-key",
                "opencode-go",
                "opencode.ai/zen/go",
            ),
            (
                "OLLAMA_BASE_URL",
                "http://localhost:11434",
                "ollama",
                "localhost:11434",
            ),
        ];

        for (env_var, env_value, provider_name, url_substr) in cases {
            let guard = EnvGuard::new();
            unsafe {
                std::env::set_var(env_var, env_value);
            }

            let config = Config::load_from_env(&guard.test_dir)
                .unwrap_or_else(|e| panic!("load_from_env failed for {env_var}: {e}"));
            drop(guard);

            let provider = config.llm.providers.get(*provider_name).unwrap_or_else(|| {
                panic!(
                    "provider '{provider_name}' not registered when '{env_var}' is set — \
                     add an .entry(\"{provider_name}\").or_insert_with(...) block in load_from_env()"
                )
            });

            assert!(
                provider.base_url.contains(url_substr),
                "provider '{provider_name}' base_url '{}' does not contain '{url_substr}'",
                provider.base_url
            );
        }
    }

    // --- Named Messaging Adapter Tests ---

    #[test]
    fn runtime_adapter_key_default() {
        assert_eq!(binding_runtime_adapter_key("telegram", None), "telegram");
    }

    #[test]
    fn runtime_adapter_key_named() {
        assert_eq!(
            binding_runtime_adapter_key("telegram", Some("support")),
            "telegram:support"
        );
    }

    #[test]
    fn runtime_adapter_key_empty_name_is_default() {
        assert_eq!(binding_runtime_adapter_key("discord", Some("")), "discord");
    }

    #[test]
    fn binding_runtime_adapter_key_method() {
        let binding = Binding {
            agent_id: "main".into(),
            channel: "telegram".into(),
            adapter: Some("sales".into()),
            guild_id: None,
            workspace_id: None,
            chat_id: None,
            channel_ids: vec![],
            require_mention: false,
            dm_allowed_users: vec![],
        };
        assert_eq!(binding.runtime_adapter_key(), "telegram:sales");
    }

    #[test]
    fn binding_uses_default_adapter() {
        let binding = Binding {
            agent_id: "main".into(),
            channel: "discord".into(),
            adapter: None,
            guild_id: None,
            workspace_id: None,
            chat_id: None,
            channel_ids: vec![],
            require_mention: false,
            dm_allowed_users: vec![],
        };
        assert!(binding.uses_default_adapter());
    }

    fn test_inbound_message(source: &str, adapter: Option<&str>) -> crate::InboundMessage {
        crate::InboundMessage {
            id: "test".into(),
            source: source.into(),
            adapter: adapter.map(String::from),
            conversation_id: "conv".into(),
            sender_id: "user1".into(),
            agent_id: None,
            content: crate::MessageContent::Text("hello".into()),
            timestamp: chrono::Utc::now(),
            metadata: Default::default(),
            formatted_author: None,
        }
    }

    #[test]
    fn adapter_matches_default_binding_default_message() {
        let binding = Binding {
            agent_id: "main".into(),
            channel: "telegram".into(),
            adapter: None,
            guild_id: None,
            workspace_id: None,
            chat_id: None,
            channel_ids: vec![],
            require_mention: false,
            dm_allowed_users: vec![],
        };
        let message = test_inbound_message("telegram", None);
        assert!(binding_adapter_matches(&binding, &message));
    }

    #[test]
    fn adapter_matches_named_binding_named_message() {
        let binding = Binding {
            agent_id: "main".into(),
            channel: "telegram".into(),
            adapter: Some("support".into()),
            guild_id: None,
            workspace_id: None,
            chat_id: None,
            channel_ids: vec![],
            require_mention: false,
            dm_allowed_users: vec![],
        };
        let message = test_inbound_message("telegram", Some("telegram:support"));
        assert!(binding_adapter_matches(&binding, &message));
    }

    #[test]
    fn adapter_mismatch_named_vs_default() {
        let binding = Binding {
            agent_id: "main".into(),
            channel: "telegram".into(),
            adapter: Some("support".into()),
            guild_id: None,
            workspace_id: None,
            chat_id: None,
            channel_ids: vec![],
            require_mention: false,
            dm_allowed_users: vec![],
        };
        let message = test_inbound_message("telegram", None);
        assert!(!binding_adapter_matches(&binding, &message));
    }

    #[test]
    fn adapter_mismatch_default_vs_named() {
        let binding = Binding {
            agent_id: "main".into(),
            channel: "telegram".into(),
            adapter: None,
            guild_id: None,
            workspace_id: None,
            chat_id: None,
            channel_ids: vec![],
            require_mention: false,
            dm_allowed_users: vec![],
        };
        let message = test_inbound_message("telegram", Some("telegram:support"));
        assert!(!binding_adapter_matches(&binding, &message));
    }

    #[test]
    fn adapter_mismatch_different_names() {
        let binding = Binding {
            agent_id: "main".into(),
            channel: "telegram".into(),
            adapter: Some("support".into()),
            guild_id: None,
            workspace_id: None,
            chat_id: None,
            channel_ids: vec![],
            require_mention: false,
            dm_allowed_users: vec![],
        };
        let message = test_inbound_message("telegram", Some("telegram:sales"));
        assert!(!binding_adapter_matches(&binding, &message));
    }

    #[test]
    fn validate_named_adapters_valid_config() {
        let messaging = MessagingConfig {
            discord: None,
            slack: None,
            telegram: Some(TelegramConfig {
                enabled: true,
                token: "tok".into(),
                instances: vec![TelegramInstanceConfig {
                    name: "support".into(),
                    enabled: true,
                    token: "tok2".into(),
                    dm_allowed_users: vec![],
                }],
                dm_allowed_users: vec![],
            }),
            email: None,
            webhook: None,
            twitch: None,
        };
        let bindings = vec![
            Binding {
                agent_id: "main".into(),
                channel: "telegram".into(),
                adapter: None,
                guild_id: None,
                workspace_id: None,
                chat_id: None,
                channel_ids: vec![],
                require_mention: false,
                dm_allowed_users: vec![],
            },
            Binding {
                agent_id: "support-agent".into(),
                channel: "telegram".into(),
                adapter: Some("support".into()),
                guild_id: None,
                workspace_id: None,
                chat_id: None,
                channel_ids: vec![],
                require_mention: false,
                dm_allowed_users: vec![],
            },
        ];
        assert!(validate_named_messaging_adapters(&messaging, &bindings).is_ok());
    }

    #[test]
    fn validate_named_adapters_missing_instance() {
        let messaging = MessagingConfig {
            discord: None,
            slack: None,
            telegram: Some(TelegramConfig {
                enabled: true,
                token: "tok".into(),
                instances: vec![],
                dm_allowed_users: vec![],
            }),
            email: None,
            webhook: None,
            twitch: None,
        };
        let bindings = vec![Binding {
            agent_id: "main".into(),
            channel: "telegram".into(),
            adapter: Some("nonexistent".into()),
            guild_id: None,
            workspace_id: None,
            chat_id: None,
            channel_ids: vec![],
            require_mention: false,
            dm_allowed_users: vec![],
        }];
        assert!(validate_named_messaging_adapters(&messaging, &bindings).is_err());
    }

    #[test]
    fn validate_named_adapters_duplicate_names_rejected() {
        let result = validate_instance_names("telegram", ["support", "support"].into_iter());
        assert!(result.is_err());
    }

    #[test]
    fn validate_named_adapters_empty_name_rejected() {
        let result = validate_instance_names("telegram", [""].into_iter());
        assert!(result.is_err());
    }

    #[test]
    fn validate_named_adapters_default_name_rejected() {
        let result = validate_instance_names("telegram", ["default"].into_iter());
        assert!(result.is_err());
    }

    #[test]
    fn validate_adapter_on_unsupported_platform_rejected() {
        let messaging = MessagingConfig {
            discord: None,
            slack: None,
            telegram: None,
            email: Some(EmailConfig {
                enabled: true,
                imap_host: "imap.test.com".into(),
                imap_port: 993,
                imap_username: "user".into(),
                imap_password: "pass".into(),
                imap_use_tls: true,
                smtp_host: "smtp.test.com".into(),
                smtp_port: 587,
                smtp_username: "user".into(),
                smtp_password: "pass".into(),
                smtp_use_starttls: true,
                from_address: "bot@test.com".into(),
                from_name: None,
                poll_interval_secs: 60,
                folders: vec![],
                allowed_senders: vec![],
                max_body_bytes: 1_000_000,
                max_attachment_bytes: 10_000_000,
                instances: vec![],
            }),
            webhook: None,
            twitch: None,
        };
        let bindings = vec![Binding {
            agent_id: "main".into(),
            channel: "email".into(),
            adapter: Some("named".into()),
            guild_id: None,
            workspace_id: None,
            chat_id: None,
            channel_ids: vec![],
            require_mention: false,
            dm_allowed_users: vec![],
        }];
        assert!(validate_named_messaging_adapters(&messaging, &bindings).is_err());
    }

    #[test]
    fn validate_binding_without_default_adapter_rejected() {
        let messaging = MessagingConfig {
            discord: None,
            slack: None,
            telegram: Some(TelegramConfig {
                enabled: true,
                token: "".into(), // no default credential
                instances: vec![TelegramInstanceConfig {
                    name: "support".into(),
                    enabled: true,
                    token: "tok".into(),
                    dm_allowed_users: vec![],
                }],
                dm_allowed_users: vec![],
            }),
            email: None,
            webhook: None,
            twitch: None,
        };
        // Binding targets default adapter, but no default credentials exist
        let bindings = vec![Binding {
            agent_id: "main".into(),
            channel: "telegram".into(),
            adapter: None,
            guild_id: None,
            workspace_id: None,
            chat_id: None,
            channel_ids: vec![],
            require_mention: false,
            dm_allowed_users: vec![],
        }];
        assert!(validate_named_messaging_adapters(&messaging, &bindings).is_err());
    }

    #[test]
    fn inbound_message_adapter_selector_default() {
        let message = test_inbound_message("telegram", None);
        assert_eq!(message.adapter_selector(), None);
    }

    #[test]
    fn inbound_message_adapter_selector_named() {
        let message = test_inbound_message("telegram", Some("telegram:support"));
        assert_eq!(message.adapter_selector(), Some("support"));
    }

    #[test]
    fn inbound_message_adapter_key_default() {
        let message = test_inbound_message("telegram", None);
        assert_eq!(message.adapter_key(), "telegram");
    }

    #[test]
    fn inbound_message_adapter_key_named() {
        let message = test_inbound_message("telegram", Some("telegram:support"));
        assert_eq!(message.adapter_key(), "telegram:support");
    }

    #[test]
    fn toml_round_trip_with_named_instances() {
        let _guard = env_test_lock().lock().unwrap();
        let guard = EnvGuard::new();

        let toml_content = r#"
[messaging.telegram]
enabled = true
token = "default-token"

[[messaging.telegram.instances]]
name = "support"
enabled = true
token = "support-token"

[[bindings]]
agent_id = "main"
channel = "telegram"

[[bindings]]
agent_id = "support-bot"
channel = "telegram"
adapter = "support"
chat_id = "-100111"
"#;
        let config_path = guard.test_dir.join("config.toml");
        std::fs::write(&config_path, toml_content).unwrap();

        let config = Config::load_from_path(&config_path).unwrap();
        let telegram = config.messaging.telegram.as_ref().unwrap();
        assert_eq!(telegram.token, "default-token");
        assert_eq!(telegram.instances.len(), 1);
        assert_eq!(telegram.instances[0].name, "support");
        assert_eq!(telegram.instances[0].token, "support-token");

        assert_eq!(config.bindings.len(), 2);
        assert!(config.bindings[0].adapter.is_none());
        assert_eq!(config.bindings[1].adapter.as_deref(), Some("support"));
        assert_eq!(config.bindings[1].chat_id.as_deref(), Some("-100111"));
    }

    #[test]
    fn toml_backward_compat_no_adapter_field() {
        let _guard = env_test_lock().lock().unwrap();
        let guard = EnvGuard::new();

        let toml_content = r#"
[messaging.discord]
enabled = true
token = "my-discord-token"

[[bindings]]
agent_id = "main"
channel = "discord"
guild_id = "123456"
"#;
        let config_path = guard.test_dir.join("config.toml");
        std::fs::write(&config_path, toml_content).unwrap();

        let config = Config::load_from_path(&config_path).unwrap();
        assert!(config.bindings[0].adapter.is_none());
        assert_eq!(config.bindings[0].guild_id.as_deref(), Some("123456"));
    }

    #[test]
    fn normalize_adapter_trims_and_clears_empty() {
        assert_eq!(normalize_adapter(None), None);
        assert_eq!(normalize_adapter(Some("".into())), None);
        assert_eq!(normalize_adapter(Some("   ".into())), None);
        assert_eq!(
            normalize_adapter(Some(" support ".into())),
            Some("support".into())
        );
        assert_eq!(normalize_adapter(Some("ops".into())), Some("ops".into()));
    }
}
