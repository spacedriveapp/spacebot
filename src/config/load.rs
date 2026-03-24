use super::providers::{
    ANTHROPIC_PROVIDER_BASE_URL, DEEPSEEK_PROVIDER_BASE_URL, FIREWORKS_PROVIDER_BASE_URL,
    GEMINI_PROVIDER_BASE_URL, GITHUB_COPILOT_DEFAULT_BASE_URL, GROQ_PROVIDER_BASE_URL,
    KILO_PROVIDER_BASE_URL, MINIMAX_CN_PROVIDER_BASE_URL, MINIMAX_PROVIDER_BASE_URL,
    MISTRAL_PROVIDER_BASE_URL, MOONSHOT_PROVIDER_BASE_URL, NVIDIA_PROVIDER_BASE_URL,
    OLLAMA_PROVIDER_BASE_URL, OPENAI_PROVIDER_BASE_URL, OPENCODE_GO_PROVIDER_BASE_URL,
    OPENCODE_ZEN_PROVIDER_BASE_URL, OPENROUTER_PROVIDER_BASE_URL, TOGETHER_PROVIDER_BASE_URL,
    XAI_PROVIDER_BASE_URL, ZAI_CODING_PLAN_BASE_URL, ZHIPU_PROVIDER_BASE_URL,
    add_shorthand_provider, infer_routing_from_providers, openrouter_extra_headers,
    resolve_routing,
};
use super::toml_schema::*;
use super::{
    AgentConfig, ApiConfig, ApiType, Binding, BrowserConfig, ChannelConfig, ClosePolicy,
    CoalesceConfig, CompactionConfig, Config, CortexConfig, CronDef, DefaultsConfig, DiscordConfig,
    DiscordInstanceConfig, EmailConfig, EmailInstanceConfig, GroupDef, HumanDef, IngestionConfig,
    LinkDef, LlmConfig, MattermostConfig, MattermostInstanceConfig, McpServerConfig, McpTransport,
    MemoryPersistenceConfig, MessagingConfig, MetricsConfig, OpenCodeConfig, ProjectsConfig,
    ProviderConfig, SignalConfig, SignalInstanceConfig, SlackCommandConfig, SlackConfig,
    SlackInstanceConfig, TelegramConfig, TelegramInstanceConfig, TelemetryConfig, TwitchConfig,
    TwitchInstanceConfig, WarmupConfig, WebhookConfig, normalize_adapter,
    validate_named_messaging_adapters,
};
use crate::error::{ConfigError, Result};

use anyhow::Context as _;

use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Resolve a value that might be an "env:VAR_NAME" or "secret:NAME" reference.
///
/// Three resolution modes:
/// - `secret:NAME` — look up from the secrets store (if available).
/// - `env:VAR_NAME` — read from system environment variable.
/// - Anything else — literal value.
pub(crate) fn resolve_env_value(value: &str) -> Option<String> {
    if let Some(alias) = value.strip_prefix("secret:") {
        let guard = RESOLVE_SECRETS_STORE.load();
        match (*guard).as_ref() {
            Some(store) => match store.get(alias) {
                Ok(secret) => Some(secret.expose().to_string()),
                Err(error) => {
                    tracing::warn!(%error, alias, "failed to resolve secret: reference");
                    None
                }
            },
            None => None,
        }
    } else if let Some(var_name) = value.strip_prefix("env:") {
        std::env::var(var_name).ok()
    } else {
        Some(value.to_string())
    }
}

/// Process-wide reference to the secrets store for use during config resolution.
///
/// Uses `ArcSwap` so it is accessible from any thread (file watcher, API
/// handlers, tokio workers) without the thread-affinity issues of a thread-local.
static RESOLVE_SECRETS_STORE: std::sync::LazyLock<
    arc_swap::ArcSwap<Option<std::sync::Arc<crate::secrets::store::SecretsStore>>>,
> = std::sync::LazyLock::new(|| arc_swap::ArcSwap::from_pointee(None));

/// Set the secrets store for config resolution (process-wide, any thread).
pub fn set_resolve_secrets_store(store: std::sync::Arc<crate::secrets::store::SecretsStore>) {
    RESOLVE_SECRETS_STORE.store(std::sync::Arc::new(Some(store)));
}

/// Known top-level keys in config.toml (must match `TomlConfig` field names).
const KNOWN_TOP_LEVEL_KEYS: &[&str] = &[
    "llm",
    "defaults",
    "agents",
    "links",
    "groups",
    "humans",
    "messaging",
    "bindings",
    "api",
    "metrics",
    "telemetry",
];

/// Pre-parse check that warns about unrecognised top-level keys in a config
/// file.  Serde's default behaviour silently drops unknown fields, which leads
/// to confusing "my setting does nothing" bugs (see issue #221).
pub(super) fn warn_unknown_config_keys(content: &str) {
    let table: toml::Table = match content.parse() {
        Ok(t) => t,
        Err(_) => return, // the typed parse will report the real error
    };

    for key in table.keys() {
        if KNOWN_TOP_LEVEL_KEYS.contains(&key.as_str()) {
            continue;
        }

        if key == "mcp_servers" || key == "mcp" {
            tracing::warn!(
                "config.toml contains top-level key `{key}` which is not recognised \
                 and will be ignored. MCP servers should be defined under [defaults] \
                 as [[defaults.mcp]] (or per-agent under [[agents.mcp]]). \
                 See docs/design-docs/mcp.md for the correct format."
            );
        } else {
            tracing::warn!(
                "config.toml contains unknown top-level key `{key}` — \
                 it will be silently ignored by the parser. Check for typos \
                 or consult the configuration reference."
            );
        }
    }
}

fn parse_close_policy(value: Option<&str>) -> Option<ClosePolicy> {
    match value? {
        "close_browser" => Some(ClosePolicy::CloseBrowser),
        "close_tabs" => Some(ClosePolicy::CloseTabs),
        "detach" => Some(ClosePolicy::Detach),
        other => {
            tracing::warn!(
                value = other,
                "unknown close_policy value, expected one of: close_browser, close_tabs, detach"
            );
            None
        }
    }
}

/// Resolve the effective close policy. When `persist_session` is enabled and no
/// explicit `close_policy` was provided, default to `Detach` so browser tabs and
/// cookies survive across workers.
fn resolve_close_policy(
    explicit: Option<&str>,
    persist_session: bool,
    fallback: ClosePolicy,
) -> ClosePolicy {
    parse_close_policy(explicit).unwrap_or(if persist_session {
        ClosePolicy::Detach
    } else {
        fallback
    })
}

impl CortexConfig {
    fn resolve(overrides: TomlCortexConfig, defaults: CortexConfig) -> Result<CortexConfig> {
        let maintenance_interval_secs = overrides
            .maintenance_interval_secs
            .unwrap_or(defaults.maintenance_interval_secs);
        if maintenance_interval_secs < 1 {
            return Err(
                ConfigError::Invalid("maintenance_interval_secs must be >= 1".to_string()).into(),
            );
        }

        let config = CortexConfig {
            tick_interval_secs: overrides
                .tick_interval_secs
                .unwrap_or(defaults.tick_interval_secs),
            worker_timeout_secs: overrides
                .worker_timeout_secs
                .unwrap_or(defaults.worker_timeout_secs),
            branch_timeout_secs: overrides
                .branch_timeout_secs
                .unwrap_or(defaults.branch_timeout_secs),
            detached_worker_timeout_retry_limit: overrides
                .detached_worker_timeout_retry_limit
                .unwrap_or(defaults.detached_worker_timeout_retry_limit),
            supervisor_kill_budget_per_tick: overrides
                .supervisor_kill_budget_per_tick
                .unwrap_or(defaults.supervisor_kill_budget_per_tick),
            circuit_breaker_threshold: overrides
                .circuit_breaker_threshold
                .unwrap_or(defaults.circuit_breaker_threshold),
            bulletin_interval_secs: overrides
                .bulletin_interval_secs
                .unwrap_or(defaults.bulletin_interval_secs),
            bulletin_max_words: overrides
                .bulletin_max_words
                .unwrap_or(defaults.bulletin_max_words),
            bulletin_max_turns: overrides
                .bulletin_max_turns
                .unwrap_or(defaults.bulletin_max_turns),
            maintenance_interval_secs,
            maintenance_decay_rate: overrides
                .maintenance_decay_rate
                .unwrap_or(defaults.maintenance_decay_rate),
            maintenance_prune_threshold: overrides
                .maintenance_prune_threshold
                .unwrap_or(defaults.maintenance_prune_threshold),
            maintenance_min_age_days: overrides
                .maintenance_min_age_days
                .unwrap_or(defaults.maintenance_min_age_days),
            maintenance_merge_similarity_threshold: overrides
                .maintenance_merge_similarity_threshold
                .unwrap_or(defaults.maintenance_merge_similarity_threshold),
            association_interval_secs: overrides
                .association_interval_secs
                .unwrap_or(defaults.association_interval_secs),
            association_similarity_threshold: overrides
                .association_similarity_threshold
                .unwrap_or(defaults.association_similarity_threshold),
            association_updates_threshold: overrides
                .association_updates_threshold
                .unwrap_or(defaults.association_updates_threshold),
            association_max_per_pass: overrides
                .association_max_per_pass
                .unwrap_or(defaults.association_max_per_pass),
            knowledge_synthesis_max_words: overrides
                .knowledge_synthesis_max_words
                .unwrap_or(defaults.knowledge_synthesis_max_words),
            knowledge_synthesis_debounce_secs: overrides
                .knowledge_synthesis_debounce_secs
                .unwrap_or(defaults.knowledge_synthesis_debounce_secs),
        };
        config.validate_maintenance_bounds()?;
        Ok(config)
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

        warn_unknown_config_keys(&content);

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
            github_copilot_key: std::env::var("GITHUB_COPILOT_API_KEY").ok(),
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
                    extra_headers: vec![],
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
                    extra_headers: openrouter_extra_headers(),
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

        add_shorthand_provider(
            &mut llm.providers,
            "github-copilot",
            llm.github_copilot_key.clone(),
            ApiType::OpenAiChatCompletions,
            GITHUB_COPILOT_DEFAULT_BASE_URL,
            Some("GitHub Copilot"),
            true,
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
                    extra_headers: vec![],
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
                    extra_headers: vec![],
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
                    extra_headers: vec![],
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
                    extra_headers: openrouter_extra_headers(),
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
                    extra_headers: vec![],
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
                    extra_headers: vec![],
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
                    extra_headers: vec![],
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
                    extra_headers: vec![],
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
                    extra_headers: vec![],
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
                    extra_headers: vec![],
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
                    extra_headers: vec![],
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
                    extra_headers: vec![],
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
                    extra_headers: vec![],
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
                    extra_headers: vec![],
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
                    extra_headers: vec![],
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
                    extra_headers: vec![],
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
                    extra_headers: vec![],
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
                    extra_headers: vec![],
                });
        }

        // Note: We allow boot without provider keys now. System starts in setup mode.
        // Agents are initialized later when keys are added via API.

        // Env-only routing: infer from configured providers, then apply env
        // overrides.  This way users who only set OPENROUTER_API_KEY get
        // openrouter/* routing instead of the hardcoded anthropic/* default.
        let mut routing = infer_routing_from_providers(&llm.providers).unwrap_or_default();
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
            gradient_start: None,
            gradient_end: None,
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
            channel: None,
            mcp: None,
            brave_search_key: None,
            cron_timezone: None,
            user_timezone: None,
            sandbox: None,
            projects: None,
            cron: Vec::new(),
        }];

        let mut api = ApiConfig::default();
        api.bind = hosted_api_bind(api.bind);

        let mut defaults = DefaultsConfig::default();
        defaults.browser.chrome_cache_dir = instance_dir.join("chrome_cache");

        Ok(Self {
            instance_dir: instance_dir.to_path_buf(),
            llm,
            defaults,
            agents,
            links: vec![LinkDef {
                from: "admin".into(),
                to: "main".into(),
                direction: "one_way".into(),
                kind: "hierarchical".into(),
            }],
            groups: Vec::new(),
            humans: vec![HumanDef {
                id: "admin".into(),
                display_name: None,
                role: None,
                bio: None,
                description: load_human_md(&instance_dir.join("humans").join("admin")),
                discord_id: None,
                telegram_id: None,
                slack_id: None,
                email: None,
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
        warn_unknown_config_keys(content);

        let toml_config: TomlConfig =
            toml::from_str(content).context("failed to parse config TOML")?;
        // Run full conversion to catch semantic errors (env resolution, defaults, etc.)
        let instance_dir = Self::default_instance_dir();
        Self::from_toml(toml_config, instance_dir)?;
        Ok(())
    }

    pub(super) fn from_toml(toml: TomlConfig, instance_dir: PathBuf) -> Result<Self> {
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
            github_copilot_key: toml
                .llm
                .github_copilot_key
                .as_deref()
                .and_then(resolve_env_value)
                .or_else(|| std::env::var("GITHUB_COPILOT_API_KEY").ok()),
            providers: toml
                .llm
                .providers
                .into_iter()
                .map(|(provider_id, config)| {
                    let api_key = resolve_env_value(&config.api_key).ok_or_else(|| {
                        anyhow::anyhow!("failed to resolve API key for provider '{}'", provider_id)
                    })?;
                    let normalized_id = provider_id.to_lowercase();
                    let extra_headers = if normalized_id == "openrouter" {
                        openrouter_extra_headers()
                    } else {
                        vec![]
                    };
                    Ok((
                        normalized_id,
                        ProviderConfig {
                            api_type: config.api_type,
                            base_url: config.base_url,
                            api_key,
                            name: config.name,
                            use_bearer_auth: false,
                            extra_headers,
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
                    extra_headers: vec![],
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
                    extra_headers: vec![],
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
                    extra_headers: openrouter_extra_headers(),
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

        add_shorthand_provider(
            &mut llm.providers,
            "github-copilot",
            llm.github_copilot_key.clone(),
            ApiType::OpenAiChatCompletions,
            GITHUB_COPILOT_DEFAULT_BASE_URL,
            Some("GitHub Copilot"),
            true,
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
                    extra_headers: vec![],
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
                    extra_headers: vec![],
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
                    extra_headers: vec![],
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
                    extra_headers: vec![],
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
                    extra_headers: vec![],
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
                    extra_headers: vec![],
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
                    extra_headers: vec![],
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
                    extra_headers: vec![],
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
                    extra_headers: vec![],
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
                    extra_headers: vec![],
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
                    extra_headers: vec![],
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
                    extra_headers: vec![],
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
        // When `[defaults.routing]` is absent, infer sane routing from the
        // first configured provider so new agents don't fall back to the
        // hardcoded `anthropic/claude-sonnet-4` default (which fails if the
        // user only has e.g. OpenRouter configured).
        let base_routing = if toml.defaults.routing.is_none() {
            infer_routing_from_providers(&llm.providers)
                .unwrap_or_else(|| base_defaults.routing.clone())
        } else {
            base_defaults.routing.clone()
        };
        let defaults = DefaultsConfig {
            routing: resolve_routing(toml.defaults.routing, &base_routing),
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
                .map(|c| CortexConfig::resolve(c, base_defaults.cortex))
                .transpose()?
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
            browser: {
                let chrome_cache_dir = instance_dir.join("chrome_cache");
                toml.defaults
                    .browser
                    .map(|b| {
                        let base = &base_defaults.browser;
                        BrowserConfig {
                            enabled: b.enabled.unwrap_or(base.enabled),
                            headless: b.headless.unwrap_or(base.headless),
                            evaluate_enabled: b.evaluate_enabled.unwrap_or(base.evaluate_enabled),
                            executable_path: b
                                .executable_path
                                .or_else(|| base.executable_path.clone()),
                            screenshot_dir: b
                                .screenshot_dir
                                .map(PathBuf::from)
                                .or_else(|| base.screenshot_dir.clone()),
                            persist_session: b.persist_session.unwrap_or(base.persist_session),
                            close_policy: resolve_close_policy(
                                b.close_policy.as_deref(),
                                b.persist_session.unwrap_or(base.persist_session),
                                base.close_policy,
                            ),
                            chrome_cache_dir: chrome_cache_dir.clone(),
                        }
                    })
                    .unwrap_or_else(|| BrowserConfig {
                        chrome_cache_dir,
                        ..base_defaults.browser.clone()
                    })
            },
            channel: toml
                .defaults
                .channel
                .map(|channel_config| ChannelConfig {
                    listen_only_mode: channel_config
                        .listen_only_mode
                        .unwrap_or(base_defaults.channel.listen_only_mode),
                    save_attachments: channel_config
                        .save_attachments
                        .unwrap_or(base_defaults.channel.save_attachments),
                })
                .unwrap_or(base_defaults.channel),
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
            projects: toml
                .defaults
                .projects
                .map(|p| {
                    let base = &base_defaults.projects;
                    ProjectsConfig {
                        use_worktrees: p.use_worktrees.unwrap_or(base.use_worktrees),
                        worktree_name_template: p
                            .worktree_name_template
                            .unwrap_or_else(|| base.worktree_name_template.clone()),
                        auto_create_worktrees: p
                            .auto_create_worktrees
                            .unwrap_or(base.auto_create_worktrees),
                        auto_discover_repos: p
                            .auto_discover_repos
                            .unwrap_or(base.auto_discover_repos),
                        auto_discover_worktrees: p
                            .auto_discover_worktrees
                            .unwrap_or(base.auto_discover_worktrees),
                        disk_usage_warning_threshold: p
                            .disk_usage_warning_threshold
                            .unwrap_or(base.disk_usage_warning_threshold),
                    }
                })
                .unwrap_or_else(|| base_defaults.projects.clone()),
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
                    gradient_start: a.gradient_start,
                    gradient_end: a.gradient_end,
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
                    cortex: a
                        .cortex
                        .map(|c| CortexConfig::resolve(c, defaults.cortex))
                        .transpose()?,
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
                        persist_session: b
                            .persist_session
                            .unwrap_or(defaults.browser.persist_session),
                        close_policy: resolve_close_policy(
                            b.close_policy.as_deref(),
                            b.persist_session
                                .unwrap_or(defaults.browser.persist_session),
                            defaults.browser.close_policy,
                        ),
                        chrome_cache_dir: defaults.browser.chrome_cache_dir.clone(),
                    }),
                    channel: a.channel.map(|channel_config| ChannelConfig {
                        listen_only_mode: channel_config
                            .listen_only_mode
                            .unwrap_or(defaults.channel.listen_only_mode),
                        save_attachments: channel_config
                            .save_attachments
                            .unwrap_or(defaults.channel.save_attachments),
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
                    projects: a.projects.map(|p| {
                        let base = &defaults.projects;
                        ProjectsConfig {
                            use_worktrees: p.use_worktrees.unwrap_or(base.use_worktrees),
                            worktree_name_template: p
                                .worktree_name_template
                                .unwrap_or_else(|| base.worktree_name_template.clone()),
                            auto_create_worktrees: p
                                .auto_create_worktrees
                                .unwrap_or(base.auto_create_worktrees),
                            auto_discover_repos: p
                                .auto_discover_repos
                                .unwrap_or(base.auto_discover_repos),
                            auto_discover_worktrees: p
                                .auto_discover_worktrees
                                .unwrap_or(base.auto_discover_worktrees),
                            disk_usage_warning_threshold: p
                                .disk_usage_warning_threshold
                                .unwrap_or(base.disk_usage_warning_threshold),
                        }
                    }),
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
                gradient_start: None,
                gradient_end: None,
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
                channel: None,
                mcp: None,
                brave_search_key: None,
                cron_timezone: None,
                user_timezone: None,
                sandbox: None,
                projects: None,
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
            signal: toml.messaging.signal.and_then(|s| {
                let instances = s
                    .instances
                    .into_iter()
                    .map(|instance| {
                        let http_url = instance.http_url.as_deref().and_then(resolve_env_value);
                        let account = instance.account.as_deref().and_then(resolve_env_value);
                        if instance.enabled && (http_url.is_none() || account.is_none()) {
                            tracing::warn!(
                                adapter = %instance.name,
                                "signal instance is enabled but http_url or account is missing/unresolvable — disabling"
                            );
                        }
                        let has_credentials = http_url.is_some() && account.is_some();
                        SignalInstanceConfig {
                            name: instance.name,
                            enabled: instance.enabled && has_credentials,
                            http_url: http_url.unwrap_or_default(),
                            account: account.unwrap_or_default(),
                            dm_allowed_users: instance.dm_allowed_users,
                            group_ids: instance.group_ids,
                            group_allowed_users: instance.group_allowed_users,
                            ignore_stories: instance.ignore_stories,
                        }
                    })
                    .collect::<Vec<_>>();

                let http_url = std::env::var("SIGNAL_HTTP_URL")
                    .ok()
                    .or_else(|| s.http_url.as_deref().and_then(resolve_env_value));
                let account = std::env::var("SIGNAL_ACCOUNT")
                    .ok()
                    .or_else(|| s.account.as_deref().and_then(resolve_env_value));

                if (http_url.is_none() || account.is_none())
                    && !instances.iter().any(|inst| inst.enabled)
                {
                    return None;
                }

                Some(SignalConfig {
                    enabled: s.enabled,
                    http_url: http_url.unwrap_or_default(),
                    account: account.unwrap_or_default(),
                    instances,
                    dm_allowed_users: s.dm_allowed_users,
                    group_ids: s.group_ids,
                    group_allowed_users: s.group_allowed_users,
                    ignore_stories: s.ignore_stories,
                })
            }),
            mattermost: toml.messaging.mattermost.and_then(|mm| {
                let instances = mm
                    .instances
                    .into_iter()
                    .map(|instance| {
                        let token = instance.token.as_deref().and_then(resolve_env_value);
                        let base_url = instance.base_url.as_deref().and_then(resolve_env_value);
                        let has_credentials = token.is_some() && base_url.is_some();
                        if instance.enabled && !has_credentials {
                            tracing::warn!(
                                adapter = %instance.name,
                                "mattermost instance is enabled but credentials are missing/unresolvable — disabling"
                            );
                        }
                        MattermostInstanceConfig {
                            name: instance.name,
                            enabled: instance.enabled && has_credentials,
                            base_url: base_url.unwrap_or_default(),
                            token: token.unwrap_or_default(),
                            team_id: instance.team_id,
                            dm_allowed_users: instance.dm_allowed_users,
                            max_attachment_bytes: instance.max_attachment_bytes,
                        }
                    })
                    .collect::<Vec<_>>();

                let token = std::env::var("MATTERMOST_TOKEN")
                    .ok()
                    .or_else(|| mm.token.as_deref().and_then(resolve_env_value));
                let base_url = std::env::var("MATTERMOST_BASE_URL")
                    .ok()
                    .or_else(|| mm.base_url.as_deref().and_then(resolve_env_value));

                if (token.is_none() || base_url.is_none()) && instances.is_empty() {
                    tracing::warn!("mattermost config present but no credentials found");
                    return None;
                }

                Some(MattermostConfig {
                    enabled: mm.enabled,
                    base_url: base_url.unwrap_or_default(),
                    token: token.unwrap_or_default(),
                    team_id: mm.team_id,
                    instances,
                    dm_allowed_users: mm.dm_allowed_users,
                    max_attachment_bytes: mm.max_attachment_bytes,
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
                team_id: b.team_id,
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

        let mut links: Vec<LinkDef> = toml
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

        let humans_dir = instance_dir.join("humans");
        let mut humans: Vec<HumanDef> = toml
            .humans
            .into_iter()
            .map(|h| {
                let description = load_human_md(&humans_dir.join(&h.id));
                HumanDef {
                    id: h.id,
                    display_name: h.display_name,
                    role: h.role,
                    bio: h.bio,
                    description,
                    discord_id: h.discord_id,
                    telegram_id: h.telegram_id,
                    slack_id: h.slack_id,
                    email: h.email,
                }
            })
            .collect();

        // Default admin human if none defined
        if humans.is_empty() {
            let description = load_human_md(&humans_dir.join("admin"));
            humans.push(HumanDef {
                id: "admin".into(),
                display_name: None,
                role: None,
                bio: None,
                description,
                discord_id: None,
                telegram_id: None,
                slack_id: None,
                email: None,
            });

            // Link the default admin to the default agent so the agent sees
            // the human's context in its system prompt automatically.
            if links.is_empty()
                && let Some(default_agent) = agents.iter().find(|a| a.default)
            {
                links.push(LinkDef {
                    from: "admin".into(),
                    to: default_agent.id.clone(),
                    direction: "one_way".into(),
                    kind: "hierarchical".into(),
                });
            }
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
}

/// Load `HUMAN.md` from a human's directory, returning `None` if the file
/// doesn't exist or is empty/whitespace.
fn load_human_md(human_dir: &std::path::Path) -> Option<String> {
    let path = human_dir.join("HUMAN.md");
    match std::fs::read_to_string(&path) {
        Ok(content) if !content.trim().is_empty() => Some(content),
        _ => None,
    }
}
