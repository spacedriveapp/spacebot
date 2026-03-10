// -- TOML deserialization types --

use serde::{Deserialize, Deserializer};
use std::collections::HashMap;

#[derive(Deserialize)]
pub(super) struct TomlConfig {
    #[serde(default)]
    pub(super) llm: TomlLlmConfig,
    #[serde(default)]
    pub(super) defaults: TomlDefaultsConfig,
    #[serde(default)]
    pub(super) agents: Vec<TomlAgentConfig>,
    #[serde(default)]
    pub(super) links: Vec<TomlLinkDef>,
    #[serde(default)]
    pub(super) groups: Vec<TomlGroupDef>,
    #[serde(default)]
    pub(super) humans: Vec<TomlHumanDef>,
    #[serde(default)]
    pub(super) messaging: TomlMessagingConfig,
    #[serde(default)]
    pub(super) bindings: Vec<TomlBinding>,
    #[serde(default)]
    pub(super) api: TomlApiConfig,
    #[serde(default)]
    pub(super) metrics: TomlMetricsConfig,
    #[serde(default)]
    pub(super) telemetry: TomlTelemetryConfig,
}

#[derive(Deserialize)]
pub(super) struct TomlLinkDef {
    pub(super) from: String,
    pub(super) to: String,
    #[serde(default = "default_link_direction")]
    pub(super) direction: String,
    #[serde(default = "default_link_kind")]
    pub(super) kind: String,
    /// Backward compat: old configs use `relationship` instead of `kind`
    #[serde(default)]
    pub(super) relationship: Option<String>,
}

pub(super) fn default_link_direction() -> String {
    "two_way".into()
}

pub(super) fn default_link_kind() -> String {
    "peer".into()
}

#[derive(Deserialize)]
pub(super) struct TomlGroupDef {
    pub(super) name: String,
    #[serde(default)]
    pub(super) agent_ids: Vec<String>,
    pub(super) color: Option<String>,
}

#[derive(Deserialize)]
pub(super) struct TomlHumanDef {
    pub(super) id: String,
    pub(super) display_name: Option<String>,
    pub(super) role: Option<String>,
    pub(super) bio: Option<String>,
    pub(super) discord_id: Option<String>,
    pub(super) telegram_id: Option<String>,
    pub(super) slack_id: Option<String>,
    pub(super) email: Option<String>,
}

#[derive(Deserialize, Default)]
pub(super) struct TomlTelemetryConfig {
    pub(super) otlp_endpoint: Option<String>,
    pub(super) otlp_headers: Option<String>,
    pub(super) service_name: Option<String>,
    pub(super) sample_rate: Option<f64>,
}

#[derive(Deserialize)]
pub(super) struct TomlApiConfig {
    #[serde(default = "default_api_enabled")]
    pub(super) enabled: bool,
    #[serde(default = "default_api_port")]
    pub(super) port: u16,
    #[serde(default = "default_api_bind")]
    pub(super) bind: String,
    #[serde(default)]
    pub(super) auth_token: Option<String>,
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

pub(super) fn default_api_enabled() -> bool {
    true
}
pub(super) fn default_api_port() -> u16 {
    19898
}
pub(super) fn default_api_bind() -> String {
    "127.0.0.1".into()
}

pub(super) fn hosted_api_bind(bind: String) -> String {
    match std::env::var("SPACEBOT_DEPLOYMENT") {
        Ok(deployment) if deployment.eq_ignore_ascii_case("hosted") => "[::]".into(),
        _ => bind,
    }
}

#[derive(Deserialize)]
pub(super) struct TomlMetricsConfig {
    #[serde(default)]
    pub(super) enabled: bool,
    #[serde(default = "default_metrics_port")]
    pub(super) port: u16,
    #[serde(default = "default_metrics_bind")]
    pub(super) bind: String,
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

pub(super) fn default_metrics_port() -> u16 {
    9090
}
pub(super) fn default_metrics_bind() -> String {
    "0.0.0.0".into()
}

#[derive(Deserialize, Debug)]
pub(super) struct TomlProviderConfig {
    pub(super) api_type: super::ApiType,
    pub(super) base_url: String,
    pub(super) api_key: String,
    pub(super) name: Option<String>,
}

#[derive(Deserialize, Default)]
pub(super) struct TomlLlmConfigFields {
    pub(super) anthropic_key: Option<String>,
    pub(super) openai_key: Option<String>,
    pub(super) openrouter_key: Option<String>,
    pub(super) kilo_key: Option<String>,
    pub(super) zhipu_key: Option<String>,
    pub(super) groq_key: Option<String>,
    pub(super) together_key: Option<String>,
    pub(super) fireworks_key: Option<String>,
    pub(super) deepseek_key: Option<String>,
    pub(super) xai_key: Option<String>,
    pub(super) mistral_key: Option<String>,
    pub(super) gemini_key: Option<String>,
    pub(super) ollama_key: Option<String>,
    pub(super) ollama_base_url: Option<String>,
    pub(super) opencode_zen_key: Option<String>,
    pub(super) opencode_go_key: Option<String>,
    pub(super) nvidia_key: Option<String>,
    pub(super) minimax_key: Option<String>,
    pub(super) minimax_cn_key: Option<String>,
    pub(super) moonshot_key: Option<String>,
    pub(super) zai_coding_plan_key: Option<String>,
    #[serde(default)]
    pub(super) providers: HashMap<String, TomlProviderConfig>,
    #[serde(default)]
    #[serde(flatten)]
    pub(super) extra: HashMap<String, toml::Value>,
}

#[derive(Default)]
pub(super) struct TomlLlmConfig {
    pub(super) anthropic_key: Option<String>,
    pub(super) openai_key: Option<String>,
    pub(super) openrouter_key: Option<String>,
    pub(super) kilo_key: Option<String>,
    pub(super) zhipu_key: Option<String>,
    pub(super) groq_key: Option<String>,
    pub(super) together_key: Option<String>,
    pub(super) fireworks_key: Option<String>,
    pub(super) deepseek_key: Option<String>,
    pub(super) xai_key: Option<String>,
    pub(super) mistral_key: Option<String>,
    pub(super) gemini_key: Option<String>,
    pub(super) ollama_key: Option<String>,
    pub(super) ollama_base_url: Option<String>,
    pub(super) opencode_zen_key: Option<String>,
    pub(super) opencode_go_key: Option<String>,
    pub(super) nvidia_key: Option<String>,
    pub(super) minimax_key: Option<String>,
    pub(super) minimax_cn_key: Option<String>,
    pub(super) moonshot_key: Option<String>,
    pub(super) zai_coding_plan_key: Option<String>,
    pub(super) providers: HashMap<String, TomlProviderConfig>,
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
pub(super) struct TomlDefaultsConfig {
    pub(super) routing: Option<TomlRoutingConfig>,
    pub(super) max_concurrent_branches: Option<usize>,
    pub(super) max_concurrent_workers: Option<usize>,
    pub(super) max_turns: Option<usize>,
    pub(super) branch_max_turns: Option<usize>,
    pub(super) context_window: Option<usize>,
    pub(super) compaction: Option<TomlCompactionConfig>,
    pub(super) memory_persistence: Option<TomlMemoryPersistenceConfig>,
    pub(super) coalesce: Option<TomlCoalesceConfig>,
    pub(super) ingestion: Option<TomlIngestionConfig>,
    pub(super) cortex: Option<TomlCortexConfig>,
    pub(super) warmup: Option<TomlWarmupConfig>,
    pub(super) browser: Option<TomlBrowserConfig>,
    pub(super) channel: Option<TomlChannelConfig>,
    #[serde(default)]
    pub(super) mcp: Vec<TomlMcpServerConfig>,
    pub(super) brave_search_key: Option<String>,
    pub(super) cron_timezone: Option<String>,
    pub(super) user_timezone: Option<String>,
    pub(super) opencode: Option<TomlOpenCodeConfig>,
    pub(super) worker_log_mode: Option<String>,
    pub(super) projects: Option<TomlProjectsConfig>,
}

#[derive(Deserialize, Default)]
pub(super) struct TomlRoutingConfig {
    pub(super) channel: Option<String>,
    pub(super) branch: Option<String>,
    pub(super) worker: Option<String>,
    pub(super) compactor: Option<String>,
    pub(super) cortex: Option<String>,
    pub(super) voice: Option<String>,
    pub(super) voice_language: Option<String>,
    pub(super) voice_translate: Option<bool>,
    pub(super) stt_provider: Option<String>,
    pub(super) rate_limit_cooldown_secs: Option<u64>,
    pub(super) channel_thinking_effort: Option<String>,
    pub(super) branch_thinking_effort: Option<String>,
    pub(super) worker_thinking_effort: Option<String>,
    pub(super) compactor_thinking_effort: Option<String>,
    pub(super) cortex_thinking_effort: Option<String>,
    #[serde(default)]
    pub(super) task_overrides: HashMap<String, String>,
    pub(super) fallbacks: Option<HashMap<String, Vec<String>>>,
}

#[derive(Deserialize)]
pub(super) struct TomlMemoryPersistenceConfig {
    pub(super) enabled: Option<bool>,
    pub(super) message_interval: Option<usize>,
}

#[derive(Deserialize)]
pub(super) struct TomlCoalesceConfig {
    pub(super) enabled: Option<bool>,
    pub(super) debounce_ms: Option<u64>,
    pub(super) max_wait_ms: Option<u64>,
    pub(super) min_messages: Option<usize>,
    pub(super) multi_user_only: Option<bool>,
}

#[derive(Deserialize)]
pub(super) struct TomlIngestionConfig {
    pub(super) enabled: Option<bool>,
    pub(super) poll_interval_secs: Option<u64>,
    pub(super) chunk_size: Option<usize>,
}

#[derive(Deserialize)]
pub(super) struct TomlCompactionConfig {
    pub(super) background_threshold: Option<f32>,
    pub(super) aggressive_threshold: Option<f32>,
    pub(super) emergency_threshold: Option<f32>,
}

#[derive(Deserialize)]
pub(super) struct TomlCortexConfig {
    pub(super) tick_interval_secs: Option<u64>,
    pub(super) worker_timeout_secs: Option<u64>,
    pub(super) branch_timeout_secs: Option<u64>,
    pub(super) detached_worker_timeout_retry_limit: Option<u8>,
    pub(super) supervisor_kill_budget_per_tick: Option<usize>,
    pub(super) circuit_breaker_threshold: Option<u8>,
    pub(super) bulletin_interval_secs: Option<u64>,
    pub(super) bulletin_max_words: Option<usize>,
    pub(super) bulletin_max_turns: Option<usize>,
    pub(super) maintenance_interval_secs: Option<u64>,
    pub(super) maintenance_decay_rate: Option<f32>,
    pub(super) maintenance_prune_threshold: Option<f32>,
    pub(super) maintenance_min_age_days: Option<i64>,
    pub(super) maintenance_merge_similarity_threshold: Option<f32>,
    pub(super) association_interval_secs: Option<u64>,
    pub(super) association_similarity_threshold: Option<f32>,
    pub(super) association_updates_threshold: Option<f32>,
    pub(super) association_max_per_pass: Option<usize>,
}

#[derive(Deserialize)]
pub(super) struct TomlWarmupConfig {
    pub(super) enabled: Option<bool>,
    pub(super) eager_embedding_load: Option<bool>,
    pub(super) refresh_secs: Option<u64>,
    pub(super) startup_delay_secs: Option<u64>,
}

#[derive(Deserialize)]
pub(super) struct TomlBrowserConfig {
    pub(super) enabled: Option<bool>,
    pub(super) headless: Option<bool>,
    pub(super) evaluate_enabled: Option<bool>,
    pub(super) executable_path: Option<String>,
    pub(super) screenshot_dir: Option<String>,
    pub(super) persist_session: Option<bool>,
    pub(super) close_policy: Option<String>,
}

#[derive(Deserialize)]
pub(super) struct TomlChannelConfig {
    pub(super) listen_only_mode: Option<bool>,
    pub(super) save_attachments: Option<bool>,
}

#[derive(Deserialize)]
pub(super) struct TomlOpenCodeConfig {
    pub(super) enabled: Option<bool>,
    pub(super) path: Option<String>,
    pub(super) max_servers: Option<usize>,
    pub(super) server_startup_timeout_secs: Option<u64>,
    pub(super) max_restart_retries: Option<u32>,
    pub(super) permissions: Option<TomlOpenCodePermissions>,
}

#[derive(Deserialize)]
pub(super) struct TomlOpenCodePermissions {
    pub(super) edit: Option<String>,
    pub(super) bash: Option<String>,
    pub(super) webfetch: Option<String>,
}

#[derive(Deserialize)]
pub(super) struct TomlProjectsConfig {
    pub(super) use_worktrees: Option<bool>,
    pub(super) worktree_name_template: Option<String>,
    pub(super) auto_create_worktrees: Option<bool>,
    pub(super) auto_discover_repos: Option<bool>,
    pub(super) auto_discover_worktrees: Option<bool>,
    pub(super) disk_usage_warning_threshold: Option<u64>,
}

#[derive(Deserialize, Clone)]
pub(super) struct TomlMcpServerConfig {
    pub(super) name: String,
    pub(super) transport: String,
    #[serde(default = "default_mcp_enabled")]
    pub(super) enabled: bool,
    pub(super) command: Option<String>,
    #[serde(default)]
    pub(super) args: Vec<String>,
    #[serde(default)]
    pub(super) env: HashMap<String, String>,
    pub(super) url: Option<String>,
    #[serde(default)]
    pub(super) headers: HashMap<String, String>,
}

pub(super) fn default_mcp_enabled() -> bool {
    true
}

#[derive(Deserialize)]
pub(super) struct TomlAgentConfig {
    pub(super) id: String,
    #[serde(default)]
    pub(super) default: bool,
    pub(super) display_name: Option<String>,
    pub(super) role: Option<String>,
    pub(super) gradient_start: Option<String>,
    pub(super) gradient_end: Option<String>,
    pub(super) workspace: Option<String>,
    pub(super) routing: Option<TomlRoutingConfig>,
    pub(super) max_concurrent_branches: Option<usize>,
    pub(super) max_concurrent_workers: Option<usize>,
    pub(super) max_turns: Option<usize>,
    pub(super) branch_max_turns: Option<usize>,
    pub(super) context_window: Option<usize>,
    pub(super) compaction: Option<TomlCompactionConfig>,
    pub(super) memory_persistence: Option<TomlMemoryPersistenceConfig>,
    pub(super) coalesce: Option<TomlCoalesceConfig>,
    pub(super) ingestion: Option<TomlIngestionConfig>,
    pub(super) cortex: Option<TomlCortexConfig>,
    pub(super) warmup: Option<TomlWarmupConfig>,
    pub(super) browser: Option<TomlBrowserConfig>,
    pub(super) channel: Option<TomlChannelConfig>,
    pub(super) mcp: Option<Vec<TomlMcpServerConfig>>,
    pub(super) brave_search_key: Option<String>,
    pub(super) cron_timezone: Option<String>,
    pub(super) user_timezone: Option<String>,
    pub(super) sandbox: Option<crate::sandbox::SandboxConfig>,
    pub(super) projects: Option<TomlProjectsConfig>,
    #[serde(default)]
    pub(super) cron: Vec<TomlCronDef>,
}

#[derive(Deserialize)]
pub(super) struct TomlCronDef {
    pub(super) id: String,
    pub(super) prompt: String,
    pub(super) cron_expr: Option<String>,
    pub(super) interval_secs: Option<u64>,
    pub(super) delivery_target: String,
    pub(super) active_start_hour: Option<u8>,
    pub(super) active_end_hour: Option<u8>,
    #[serde(default = "default_enabled")]
    pub(super) enabled: bool,
    #[serde(default)]
    pub(super) run_once: bool,
    pub(super) timeout_secs: Option<u64>,
}

pub(super) fn default_enabled() -> bool {
    true
}

#[derive(Deserialize, Default)]
pub(super) struct TomlMessagingConfig {
    pub(super) discord: Option<TomlDiscordConfig>,
    pub(super) slack: Option<TomlSlackConfig>,
    pub(super) telegram: Option<TomlTelegramConfig>,
    pub(super) email: Option<TomlEmailConfig>,
    pub(super) webhook: Option<TomlWebhookConfig>,
    pub(super) twitch: Option<TomlTwitchConfig>,
    pub(super) signal: Option<TomlSignalConfig>,
}

#[derive(Deserialize)]
pub(super) struct TomlDiscordConfig {
    #[serde(default)]
    pub(super) enabled: bool,
    pub(super) token: Option<String>,
    #[serde(default)]
    pub(super) instances: Vec<TomlDiscordInstanceConfig>,
    #[serde(default)]
    pub(super) dm_allowed_users: Vec<String>,
    #[serde(default)]
    pub(super) allow_bot_messages: bool,
}

#[derive(Deserialize)]
pub(super) struct TomlDiscordInstanceConfig {
    pub(super) name: String,
    #[serde(default)]
    pub(super) enabled: bool,
    pub(super) token: Option<String>,
    #[serde(default)]
    pub(super) dm_allowed_users: Vec<String>,
    #[serde(default)]
    pub(super) allow_bot_messages: bool,
}

#[derive(Deserialize)]
pub(super) struct TomlSlackConfig {
    #[serde(default)]
    pub(super) enabled: bool,
    pub(super) bot_token: Option<String>,
    pub(super) app_token: Option<String>,
    #[serde(default)]
    pub(super) instances: Vec<TomlSlackInstanceConfig>,
    #[serde(default)]
    pub(super) dm_allowed_users: Vec<String>,
    #[serde(default)]
    pub(super) commands: Vec<TomlSlackCommandConfig>,
}

#[derive(Deserialize)]
pub(super) struct TomlSlackInstanceConfig {
    pub(super) name: String,
    #[serde(default)]
    pub(super) enabled: bool,
    pub(super) bot_token: Option<String>,
    pub(super) app_token: Option<String>,
    #[serde(default)]
    pub(super) dm_allowed_users: Vec<String>,
    #[serde(default)]
    pub(super) commands: Vec<TomlSlackCommandConfig>,
}

#[derive(Deserialize)]
pub(super) struct TomlSlackCommandConfig {
    pub(super) command: String,
    pub(super) agent_id: String,
    pub(super) description: Option<String>,
}

#[derive(Deserialize)]
pub(super) struct TomlTelegramConfig {
    #[serde(default)]
    pub(super) enabled: bool,
    pub(super) token: Option<String>,
    #[serde(default)]
    pub(super) instances: Vec<TomlTelegramInstanceConfig>,
    #[serde(default)]
    pub(super) dm_allowed_users: Vec<String>,
}

#[derive(Deserialize)]
pub(super) struct TomlTelegramInstanceConfig {
    pub(super) name: String,
    #[serde(default)]
    pub(super) enabled: bool,
    pub(super) token: Option<String>,
    #[serde(default)]
    pub(super) dm_allowed_users: Vec<String>,
}

#[derive(Deserialize)]
pub(super) struct TomlEmailConfig {
    #[serde(default)]
    pub(super) enabled: bool,
    pub(super) imap_host: Option<String>,
    #[serde(default = "default_email_imap_port")]
    pub(super) imap_port: u16,
    pub(super) imap_username: Option<String>,
    pub(super) imap_password: Option<String>,
    #[serde(default = "default_email_imap_use_tls")]
    pub(super) imap_use_tls: bool,
    pub(super) smtp_host: Option<String>,
    #[serde(default = "default_email_smtp_port")]
    pub(super) smtp_port: u16,
    pub(super) smtp_username: Option<String>,
    pub(super) smtp_password: Option<String>,
    #[serde(default = "default_email_smtp_use_starttls")]
    pub(super) smtp_use_starttls: bool,
    pub(super) from_address: Option<String>,
    pub(super) from_name: Option<String>,
    #[serde(default = "default_email_poll_interval_secs")]
    pub(super) poll_interval_secs: u64,
    #[serde(default = "default_email_folders")]
    pub(super) folders: Vec<String>,
    #[serde(default)]
    pub(super) allowed_senders: Vec<String>,
    #[serde(default = "default_email_max_body_bytes")]
    pub(super) max_body_bytes: usize,
    #[serde(default = "default_email_max_attachment_bytes")]
    pub(super) max_attachment_bytes: usize,
    #[serde(default)]
    pub(super) instances: Vec<TomlEmailInstanceConfig>,
}

#[derive(Deserialize)]
pub(super) struct TomlEmailInstanceConfig {
    pub(super) name: String,
    #[serde(default)]
    pub(super) enabled: bool,
    pub(super) imap_host: Option<String>,
    #[serde(default = "default_email_imap_port")]
    pub(super) imap_port: u16,
    pub(super) imap_username: Option<String>,
    pub(super) imap_password: Option<String>,
    #[serde(default = "default_email_imap_use_tls")]
    pub(super) imap_use_tls: bool,
    pub(super) smtp_host: Option<String>,
    #[serde(default = "default_email_smtp_port")]
    pub(super) smtp_port: u16,
    pub(super) smtp_username: Option<String>,
    pub(super) smtp_password: Option<String>,
    #[serde(default = "default_email_smtp_use_starttls")]
    pub(super) smtp_use_starttls: bool,
    pub(super) from_address: Option<String>,
    pub(super) from_name: Option<String>,
    #[serde(default = "default_email_poll_interval_secs")]
    pub(super) poll_interval_secs: u64,
    #[serde(default = "default_email_folders")]
    pub(super) folders: Vec<String>,
    #[serde(default)]
    pub(super) allowed_senders: Vec<String>,
    #[serde(default = "default_email_max_body_bytes")]
    pub(super) max_body_bytes: usize,
    #[serde(default = "default_email_max_attachment_bytes")]
    pub(super) max_attachment_bytes: usize,
}

#[derive(Deserialize)]
pub(super) struct TomlWebhookConfig {
    #[serde(default)]
    pub(super) enabled: bool,
    #[serde(default = "default_webhook_port")]
    pub(super) port: u16,
    #[serde(default = "default_webhook_bind")]
    pub(super) bind: String,
    pub(super) auth_token: Option<String>,
}

#[derive(Deserialize)]
pub(super) struct TomlTwitchConfig {
    #[serde(default)]
    pub(super) enabled: bool,
    pub(super) username: Option<String>,
    pub(super) oauth_token: Option<String>,
    pub(super) client_id: Option<String>,
    pub(super) client_secret: Option<String>,
    pub(super) refresh_token: Option<String>,
    #[serde(default)]
    pub(super) instances: Vec<TomlTwitchInstanceConfig>,
    #[serde(default)]
    pub(super) channels: Vec<String>,
    pub(super) trigger_prefix: Option<String>,
}

#[derive(Deserialize)]
pub(super) struct TomlTwitchInstanceConfig {
    pub(super) name: String,
    #[serde(default)]
    pub(super) enabled: bool,
    pub(super) username: Option<String>,
    pub(super) oauth_token: Option<String>,
    pub(super) client_id: Option<String>,
    pub(super) client_secret: Option<String>,
    pub(super) refresh_token: Option<String>,
    #[serde(default)]
    pub(super) channels: Vec<String>,
    pub(super) trigger_prefix: Option<String>,
}

#[derive(Deserialize)]
pub(super) struct TomlSignalConfig {
    #[serde(default)]
    pub(super) enabled: bool,
    pub(super) http_url: Option<String>,
    pub(super) account: Option<String>,
    #[serde(default)]
    pub(super) instances: Vec<TomlSignalInstanceConfig>,
    #[serde(default)]
    pub(super) dm_allowed_users: Vec<String>,
    #[serde(default)]
    pub(super) group_ids: Vec<String>,
    #[serde(default)]
    pub(super) group_allowed_users: Vec<String>,
    #[serde(default = "default_signal_ignore_stories")]
    pub(super) ignore_stories: bool,
}

#[derive(Deserialize)]
pub(super) struct TomlSignalInstanceConfig {
    pub(super) name: String,
    #[serde(default)]
    pub(super) enabled: bool,
    pub(super) http_url: Option<String>,
    pub(super) account: Option<String>,
    #[serde(default)]
    pub(super) dm_allowed_users: Vec<String>,
    #[serde(default)]
    pub(super) group_ids: Vec<String>,
    #[serde(default)]
    pub(super) group_allowed_users: Vec<String>,
    #[serde(default = "default_signal_ignore_stories")]
    pub(super) ignore_stories: bool,
}

pub(super) fn default_signal_ignore_stories() -> bool {
    true
}

pub(super) fn default_webhook_port() -> u16 {
    18789
}
pub(super) fn default_webhook_bind() -> String {
    "127.0.0.1".into()
}

pub(super) fn default_email_imap_port() -> u16 {
    993
}

pub(super) fn default_email_imap_use_tls() -> bool {
    true
}

pub(super) fn default_email_smtp_port() -> u16 {
    587
}

pub(super) fn default_email_smtp_use_starttls() -> bool {
    true
}

pub(super) fn default_email_poll_interval_secs() -> u64 {
    30
}

pub(super) fn default_email_folders() -> Vec<String> {
    vec!["INBOX".to_string()]
}

pub(super) fn default_email_max_body_bytes() -> usize {
    256 * 1024
}

pub(super) fn default_email_max_attachment_bytes() -> usize {
    10 * 1024 * 1024
}

#[derive(Deserialize)]
pub(super) struct TomlBinding {
    pub(super) agent_id: String,
    pub(super) channel: String,
    #[serde(default)]
    pub(super) adapter: Option<String>,
    pub(super) guild_id: Option<String>,
    pub(super) workspace_id: Option<String>,
    pub(super) chat_id: Option<String>,
    #[serde(default)]
    pub(super) channel_ids: Vec<String>,
    #[serde(default)]
    pub(super) require_mention: bool,
    #[serde(default)]
    pub(super) dm_allowed_users: Vec<String>,
}
