//! Configuration loading and validation.

mod load;
mod onboarding;
mod permissions;
mod providers;
mod runtime;
mod toml_schema;
mod types;
mod watcher;

// Re-export all public types from submodules so external consumers
// continue to use `crate::config::TypeName` unchanged.
pub(crate) use load::resolve_env_value;
pub use load::set_resolve_secrets_store;
pub use onboarding::run_onboarding;
pub use permissions::{
    DiscordPermissions, SlackPermissions, TelegramPermissions, TwitchPermissions,
};
pub(crate) use providers::default_provider_config;
pub use runtime::RuntimeConfig;
pub use types::*;
pub use watcher::spawn_file_watcher;

// Re-export pub(crate) items that need crate-wide visibility.
// (GEMINI_PROVIDER_BASE_URL is only used within config submodules, no re-export needed.)

// Make toml_schema types and internal helpers visible to tests in this module.
#[cfg(test)]
use load::warn_unknown_config_keys;
#[cfg(test)]
use providers::ANTHROPIC_PROVIDER_BASE_URL;
#[cfg(test)]
use providers::OPENAI_PROVIDER_BASE_URL;
#[cfg(test)]
use providers::OPENROUTER_PROVIDER_BASE_URL;
#[cfg(test)]
use toml_schema::*;
#[cfg(test)]
use types::binding_adapter_matches;
#[cfg(test)]
use types::validate_instance_names;

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::result::Result as StdResult;

    fn env_test_lock() -> &'static parking_lot::Mutex<()> {
        static LOCK: std::sync::OnceLock<parking_lot::Mutex<()>> = std::sync::OnceLock::new();
        LOCK.get_or_init(|| parking_lot::Mutex::new(()))
    }

    struct EnvGuard {
        vars: Vec<(&'static str, Option<String>)>,
        test_dir: PathBuf,
    }

    impl EnvGuard {
        fn new() -> Self {
            // NOTE: Keep in sync with provider env vars that affect test behavior
            const KEYS: [&str; 27] = [
                "SPACEBOT_DIR",
                "SPACEBOT_DEPLOYMENT",
                "SPACEBOT_CRON_TIMEZONE",
                "SPACEBOT_USER_TIMEZONE",
                "ANTHROPIC_API_KEY",
                "ANTHROPIC_BASE_URL",
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
        let _lock = env_test_lock().lock();
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
        let _lock = env_test_lock().lock();
        let _env = EnvGuard::new();

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
        assert!(
            anthropic_provider.extra_headers.is_empty(),
            "anthropic provider should have no extra_headers"
        );

        let openai_provider = config
            .llm
            .providers
            .get("openai")
            .expect("openai provider missing");
        assert_eq!(openai_provider.api_type, ApiType::OpenAiCompletions);
        assert_eq!(openai_provider.base_url, OPENAI_PROVIDER_BASE_URL);
        assert_eq!(openai_provider.api_key, "legacy-openai-key");
        assert!(
            openai_provider.extra_headers.is_empty(),
            "openai provider should have no extra_headers"
        );

        let openrouter_provider = config
            .llm
            .providers
            .get("openrouter")
            .expect("openrouter provider missing");
        assert_eq!(openrouter_provider.api_type, ApiType::OpenAiCompletions);
        assert_eq!(openrouter_provider.base_url, OPENROUTER_PROVIDER_BASE_URL);
        assert_eq!(openrouter_provider.api_key, "legacy-openrouter-key");
        assert_eq!(openrouter_provider.extra_headers.len(), 4);
        let find_header = |name: &str| -> Option<&str> {
            openrouter_provider
                .extra_headers
                .iter()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.as_str())
        };
        assert_eq!(find_header("HTTP-Referer"), Some("https://spacebot.sh/"));
        assert_eq!(find_header("X-Title"), Some("Spacebot"));
        assert_eq!(find_header("X-OpenRouter-Title"), Some("Spacebot"));
        assert_eq!(
            find_header("X-OpenRouter-Categories"),
            Some("cloud-agent,cli-agent")
        );
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
    fn test_explicit_openrouter_provider_toml_injects_extra_headers() {
        let toml = r#"
[llm.provider.openrouter]
api_type = "openai_completions"
base_url = "https://openrouter.ai/api/v1"
api_key = "explicit-openrouter-key"
name = "My OpenRouter"
"#;

        let parsed: TomlConfig = toml::from_str(toml).expect("failed to parse test TOML");
        let config = Config::from_toml(parsed, PathBuf::from(".")).expect("failed to build Config");

        let openrouter_provider = config
            .llm
            .providers
            .get("openrouter")
            .expect("openrouter provider missing");
        assert_eq!(openrouter_provider.api_type, ApiType::OpenAiCompletions);
        assert_eq!(openrouter_provider.base_url, "https://openrouter.ai/api/v1");
        assert_eq!(openrouter_provider.api_key, "explicit-openrouter-key");
        assert_eq!(openrouter_provider.name.as_deref(), Some("My OpenRouter"));

        // Verify attribution headers are injected even for explicit TOML config
        assert_eq!(openrouter_provider.extra_headers.len(), 4);
        let find_header = |name: &str| -> Option<&str> {
            openrouter_provider
                .extra_headers
                .iter()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.as_str())
        };
        assert_eq!(find_header("HTTP-Referer"), Some("https://spacebot.sh/"));
        assert_eq!(find_header("X-Title"), Some("Spacebot"));
        assert_eq!(find_header("X-OpenRouter-Title"), Some("Spacebot"));
        assert_eq!(
            find_header("X-OpenRouter-Categories"),
            Some("cloud-agent,cli-agent")
        );
    }

    #[test]
    fn test_needs_onboarding_without_config_or_env() {
        let _lock = env_test_lock().lock();
        let _env = EnvGuard::new();

        assert!(Config::needs_onboarding());
    }

    #[test]
    fn test_needs_onboarding_with_anthropic_env_key() {
        let _lock = env_test_lock().lock();
        let _env = EnvGuard::new();

        unsafe {
            std::env::set_var("ANTHROPIC_API_KEY", "test-key");
        }

        assert!(!Config::needs_onboarding());
    }

    #[test]
    fn test_needs_onboarding_false_with_oauth_credentials() {
        let _lock = env_test_lock().lock();
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
        let _lock = env_test_lock().lock();
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
        let _lock = env_test_lock().lock();
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
        let _lock = env_test_lock().lock();
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
        let _lock = env_test_lock().lock();
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
        let _lock = env_test_lock().lock();
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
        let _lock = env_test_lock().lock();
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
        let _lock = env_test_lock().lock();
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
        let _lock = env_test_lock().lock();
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
        let _lock = env_test_lock().lock();
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
        let _lock = env_test_lock().lock();
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
        let _lock = env_test_lock().lock();
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
    fn test_cortex_default_and_agent_override_resolution() {
        let toml = r#"
[defaults.cortex]
tick_interval_secs = 45
detached_worker_timeout_retry_limit = 4
supervisor_kill_budget_per_tick = 12
bulletin_max_words = 1200

[[agents]]
id = "main"

[agents.cortex]
branch_timeout_secs = 77
supervisor_kill_budget_per_tick = 3
association_max_per_pass = 55
"#;
        let parsed: TomlConfig = toml::from_str(toml).expect("failed to parse test TOML");
        let config = Config::from_toml(parsed, PathBuf::from(".")).expect("failed to build Config");
        let resolved = config.agents[0].resolve(&config.instance_dir, &config.defaults);

        assert_eq!(config.defaults.cortex.tick_interval_secs, 45);
        assert_eq!(
            config.defaults.cortex.detached_worker_timeout_retry_limit,
            4
        );
        assert_eq!(config.defaults.cortex.supervisor_kill_budget_per_tick, 12);
        assert_eq!(config.defaults.cortex.bulletin_max_words, 1200);

        assert_eq!(resolved.cortex.tick_interval_secs, 45);
        assert_eq!(resolved.cortex.branch_timeout_secs, 77);
        assert_eq!(resolved.cortex.detached_worker_timeout_retry_limit, 4);
        assert_eq!(resolved.cortex.supervisor_kill_budget_per_tick, 3);
        assert_eq!(resolved.cortex.bulletin_max_words, 1200);
        assert_eq!(resolved.cortex.association_max_per_pass, 55);
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
        let _lock = env_test_lock().lock();
        let _env = EnvGuard::new();

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
        let _lock = env_test_lock().lock();

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
        let _guard = env_test_lock().lock();
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
        let _guard = env_test_lock().lock();
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

    #[test]
    fn warn_unknown_config_keys_no_panic() {
        // Smoke test: the function should not panic for any input shape.
        // Actual warning output goes through tracing (not asserted here).
        let toml_with_mcp_servers = r#"
[[mcp_servers]]
name = "test"
transport = "stdio"
command = "/usr/bin/test"
"#;
        warn_unknown_config_keys(toml_with_mcp_servers);

        // Top-level `mcp` should also be caught
        let toml_with_mcp = r#"
[[mcp]]
name = "test"
transport = "stdio"
command = "/usr/bin/test"
"#;
        warn_unknown_config_keys(toml_with_mcp);

        // Generic unknown key
        let toml_with_unknown = r#"
[foobar]
something = true
"#;
        warn_unknown_config_keys(toml_with_unknown);

        // Valid keys should not warn
        let toml_valid = r#"
[llm]
[defaults]
[messaging]
[api]
"#;
        warn_unknown_config_keys(toml_valid);
    }

    #[test]
    fn top_level_mcp_servers_silently_ignored_by_serde() {
        // Demonstrates the root cause of issue #221: serde drops unknown fields.
        // `[[mcp_servers]]` at the top level deserializes fine but the data is lost.
        let toml = r#"
[[agents]]
id = "test-agent"

[[mcp_servers]]
name = "my-server"
transport = "stdio"
command = "/usr/bin/test"
"#;
        let parsed: TomlConfig = toml::from_str(toml).expect("should parse without error");
        // The mcp_servers data is silently dropped — verify it's not accessible
        assert!(parsed.defaults.mcp.is_empty());
    }

    #[test]
    fn qmd_mcp_server_name_is_canonicalized_on_load() {
        let toml = r#"
[[defaults.mcp]]
name = "QMD"
transport = "http"
url = "https://example.com/mcp"
"#;

        let parsed: TomlConfig = toml::from_str(toml).expect("failed to parse test TOML");
        let config = Config::from_toml(parsed, PathBuf::from(".")).expect("failed to build Config");

        assert_eq!(config.defaults.mcp.len(), 1);
        assert_eq!(config.defaults.mcp[0].name, "qmd");
    }
}
