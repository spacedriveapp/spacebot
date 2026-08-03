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
    DiscordPermissions, MattermostPermissions, SignalPermissions, SlackPermissions,
    TelegramPermissions, TwitchPermissions,
};
pub(crate) use providers::ANTHROPIC_PROVIDER_BASE_URL;
pub use runtime::RuntimeConfig;
pub use types::*;
pub use watcher::spawn_file_watcher;

// Make toml_schema types and internal helpers visible to tests in this module.
#[cfg(test)]
use load::warn_unknown_config_keys;
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
            const KEYS: &[&str] = &[
                "SPACEBOT_DIR",
                "SPACEBOT_DEPLOYMENT",
                "SPACEBOT_CRON_TIMEZONE",
                "SPACEBOT_USER_TIMEZONE",
                "ANTHROPIC_API_KEY",
                "ANTHROPIC_BASE_URL",
                "ANTHROPIC_AUTH_TOKEN",
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
                "GITHUB_COPILOT_API_KEY",
                "LITELLM_API_KEY",
                "LITELLM_BASE_URL",
            ];

            let vars = KEYS
                .iter()
                .map(|&key| (key, std::env::var(key).ok()))
                .collect::<Vec<_>>();

            for &key in KEYS {
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
    fn api_type_accepts_the_two_canonical_names() {
        let parse = |api_type: &str| -> StdResult<TomlProviderConfig, toml::de::Error> {
            toml::from_str(&format!(
                "api_type = \"{api_type}\"\nbase_url = \"https://example.com\"\napi_key = \"k\"\n"
            ))
        };

        assert_eq!(
            parse("anthropic").expect("anthropic").api_type.api_type,
            ApiType::Anthropic
        );
        assert_eq!(
            parse("openai_compatible")
                .expect("openai_compatible")
                .api_type
                .api_type,
            ApiType::OpenAiCompatible
        );
    }

    /// Retired spellings keep parsing for one release. Three of them used to
    /// append `/v1` internally, so collapsing them without rewriting base_url
    /// would silently 404 every existing config.
    #[test]
    fn retired_api_types_migrate_instead_of_breaking() {
        let parse = |api_type: &str| -> TomlProviderConfig {
            toml::from_str(&format!(
                "api_type = \"{api_type}\"\nbase_url = \"https://gateway.example\"\napi_key = \"k\"\n"
            ))
            .unwrap_or_else(|error| panic!("{api_type} should still parse: {error}"))
        };

        for retired in ["openai_completions", "gemini", "kilo_gateway"] {
            let config = parse(retired);
            assert_eq!(config.api_type.api_type, ApiType::OpenAiCompatible);
            let (base_url, warning) = config
                .api_type
                .migrate_base_url(&config.base_url, "gateway");
            assert_eq!(
                base_url, "https://gateway.example/v1",
                "{retired} must keep its /v1 path segment"
            );
            assert!(warning.is_some(), "{retired} should warn");
        }

        // This one already meant `{base_url}/chat/completions`, so the URL is
        // left alone.
        let config = parse("openai_chat_completions");
        assert_eq!(config.api_type.api_type, ApiType::OpenAiCompatible);
        let (base_url, warning) = config
            .api_type
            .migrate_base_url(&config.base_url, "gateway");
        assert_eq!(base_url, "https://gateway.example");
        assert!(warning.is_some());
    }

    /// A config already carrying `/v1` must not end up with `/v1/v1`.
    #[test]
    fn retired_api_type_migration_is_idempotent() {
        let config: TomlProviderConfig = toml::from_str(
            "api_type = \"openai_completions\"\nbase_url = \"https://host/v1\"\napi_key = \"k\"\n",
        )
        .expect("parses");
        let (base_url, _) = config
            .api_type
            .migrate_base_url(&config.base_url, "gateway");
        assert_eq!(base_url, "https://host/v1");
    }

    /// End-to-end version of the above: the full config block as the live
    /// preview and production instances have it on disk must still load, and
    /// must resolve to the same URL the retired `openai_completions` arm hit.
    #[test]
    fn live_deployment_litellm_config_still_loads() {
        let _lock = env_test_lock().lock();
        let _env = EnvGuard::new();

        unsafe {
            std::env::set_var("LITE_LLM_KEY", "test-litellm-key");
        }

        let toml_source = r#"
[llm.provider.litellm]
api_type = "openai_completions"
base_url = "https://litellm.lashwing.dev"
api_key = "env:LITE_LLM_KEY"
name = "litellm"

[defaults.routing]
channel = "litellm/Deepseek v4 Flash"

[[agents]]
id = "main"
default = true
"#;

        let parsed: TomlConfig = toml::from_str(toml_source).expect("failed to parse test TOML");
        let config = Config::from_toml(parsed, PathBuf::from(".")).expect("live config must load");

        let provider = config
            .llm
            .providers
            .get("litellm")
            .expect("litellm provider missing");
        assert_eq!(provider.api_type, ApiType::OpenAiCompatible);
        assert_eq!(provider.base_url, "https://litellm.lashwing.dev/v1");
        assert_eq!(provider.api_key, "test-litellm-key");
        assert_eq!(provider.name.as_deref(), Some("litellm"));

        unsafe {
            std::env::remove_var("LITE_LLM_KEY");
        }
    }

    /// The preview and production deployments both use this exact shape.
    #[test]
    fn litellm_openai_completions_config_keeps_its_endpoint() {
        let config: TomlProviderConfig = toml::from_str(
            "api_type = \"openai_completions\"\n             base_url = \"https://litellm.lashwing.dev\"\n             api_key = \"env:LITE_LLM_KEY\"\n",
        )
        .expect("existing deployment config must still parse");

        let (base_url, _) = config
            .api_type
            .migrate_base_url(&config.base_url, "litellm");

        // stream_openai appends `/chat/completions`, so this resolves to the
        // same URL the old OpenAiCompletions arm produced.
        assert_eq!(base_url, "https://litellm.lashwing.dev/v1");
    }

    #[test]
    fn removed_api_types_fail_with_a_migration_message() {
        for (removed, expected) in [
            ("azure", "openai_compatible"),
            ("openai_responses", "openai_compatible"),
        ] {
            let result: StdResult<TomlProviderConfig, toml::de::Error> = toml::from_str(&format!(
                "api_type = \"{removed}\"\nbase_url = \"https://example.com\"\napi_key = \"k\"\n"
            ));
            let error = result.expect_err("{removed} must be rejected").to_string();
            assert!(error.contains("has been removed"), "{removed}: {error}");
            assert!(error.contains(expected), "{removed}: {error}");
        }
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
        assert_eq!(config.api_type.api_type, ApiType::Anthropic);
        assert_eq!(config.base_url, "https://api.anthropic.com/v1");
        assert_eq!(config.api_key, "sk-ant-api03-abc123");
        assert_eq!(config.name, Some("Anthropic".to_string()));
    }

    #[test]
    fn test_provider_config_deserialization_no_name() {
        let toml = r#"
api_type = "openai_compatible"
base_url = "https://api.openai.com/v1"
api_key = "sk-proj-xyz789"
"#;
        let result: StdResult<TomlProviderConfig, toml::de::Error> = toml::from_str(toml);
        assert!(result.is_ok());
        let config = result.unwrap();
        assert_eq!(config.api_type.api_type, ApiType::OpenAiCompatible);
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
api_type = "openai_compatible"
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
        assert_eq!(my_provider.api_type, ApiType::OpenAiCompatible);
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

    /// The `llm.<provider>_key` shorthands each implied a hidden base URL and
    /// API dialect. Silently ignoring them would leave a working-looking config
    /// with zero providers, so they now fail the load with a migration message.
    #[test]
    fn retired_llm_shorthand_keys_fail_loudly() {
        let toml = r#"
[llm]
anthropic_key = "legacy-anthropic-key"
openrouter_key = "legacy-openrouter-key"
"#;

        let parsed: TomlConfig = toml::from_str(toml).expect("failed to parse test TOML");
        let error = Config::from_toml(parsed, PathBuf::from("."))
            .expect_err("retired keys must not be silently dropped")
            .to_string();

        assert!(error.contains("anthropic_key"), "{error}");
        assert!(error.contains("openrouter_key"), "{error}");
        assert!(error.contains("llm.provider"), "{error}");
    }

    /// Per-provider headers used to be hardcoded by provider name (OpenRouter
    /// attribution, Kilo gateway headers). They are now plain config.
    #[test]
    fn extra_headers_come_from_config_not_from_the_provider_name() {
        let toml = r#"
[llm.provider.openrouter]
api_type = "openai_compatible"
base_url = "https://openrouter.ai/api/v1"
api_key = "explicit-openrouter-key"
name = "My OpenRouter"
extra_headers = { "HTTP-Referer" = "https://spacebot.sh/", "X-Title" = "Spacebot" }
"#;

        let parsed: TomlConfig = toml::from_str(toml).expect("failed to parse test TOML");
        let config = Config::from_toml(parsed, PathBuf::from(".")).expect("failed to build Config");

        let provider = config
            .llm
            .providers
            .get("openrouter")
            .expect("openrouter provider missing");
        assert_eq!(provider.api_type, ApiType::OpenAiCompatible);
        assert_eq!(provider.base_url, "https://openrouter.ai/api/v1");
        assert_eq!(provider.name.as_deref(), Some("My OpenRouter"));
        assert_eq!(
            provider.extra_headers,
            vec![
                (
                    "HTTP-Referer".to_string(),
                    "https://spacebot.sh/".to_string()
                ),
                ("X-Title".to_string(), "Spacebot".to_string()),
            ]
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

    /// `ANTHROPIC_API_KEY` and `LITELLM_API_KEY` are the only two env vars that
    /// still bootstrap a provider without a config file.
    #[test]
    fn env_only_boot_registers_anthropic_and_litellm_providers() {
        let _lock = env_test_lock().lock();
        let _env = EnvGuard::new();

        unsafe {
            std::env::set_var("ANTHROPIC_API_KEY", "env-anthropic-key");
            std::env::set_var("LITELLM_API_KEY", "env-litellm-key");
            std::env::set_var("LITELLM_BASE_URL", "https://litellm.example/v1");
        }

        let config = Config::load_from_env(&PathBuf::from(".")).expect("failed to load env config");

        let anthropic = config
            .llm
            .providers
            .get("anthropic")
            .expect("anthropic provider missing");
        assert_eq!(anthropic.api_type, ApiType::Anthropic);
        assert_eq!(anthropic.base_url, ANTHROPIC_PROVIDER_BASE_URL);
        assert_eq!(anthropic.api_key, "env-anthropic-key");

        let litellm = config
            .llm
            .providers
            .get("litellm")
            .expect("litellm provider missing");
        assert_eq!(litellm.api_type, ApiType::OpenAiCompatible);
        assert_eq!(litellm.base_url, "https://litellm.example/v1");
        assert_eq!(litellm.api_key, "env-litellm-key");
    }

    /// A deployment that only sets a retired var must not boot as if it were
    /// configured — otherwise every LLM call fails at runtime instead.
    #[test]
    fn retired_provider_env_vars_do_not_register_a_provider() {
        let _lock = env_test_lock().lock();
        let _env = EnvGuard::new();

        unsafe {
            std::env::set_var("OPENROUTER_API_KEY", "env-openrouter-key");
        }

        let config = Config::load_from_env(&PathBuf::from(".")).expect("failed to load env config");
        assert!(
            config.llm.providers.is_empty(),
            "OPENROUTER_API_KEY must no longer configure a provider"
        );
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

    #[test]
    fn test_docker_deployment_forces_api_bind_from_toml() {
        let _lock = env_test_lock().lock();
        let _env = EnvGuard::new();

        unsafe {
            std::env::set_var("SPACEBOT_DEPLOYMENT", "docker");
        }

        let toml = r#"
[api]
bind = "127.0.0.1"
"#;

        let parsed: TomlConfig = toml::from_str(toml).expect("failed to parse test TOML");
        let config = Config::from_toml(parsed, PathBuf::from(".")).expect("failed to build Config");

        assert_eq!(config.api.bind, "0.0.0.0");
    }

    #[test]
    fn test_docker_deployment_forces_api_bind_from_env_defaults() {
        let _lock = env_test_lock().lock();
        let _env = EnvGuard::new();

        unsafe {
            std::env::set_var("SPACEBOT_DEPLOYMENT", "docker");
        }

        let config = Config::load_from_env(&Config::default_instance_dir())
            .expect("failed to load config from env");

        assert_eq!(config.api.bind, "0.0.0.0");
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
            team_id: None,
            channel_ids: vec![],
            require_mention: false,
            dm_allowed_users,
            settings: None,
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
maintenance_interval_secs = 1200
maintenance_prune_threshold = 0.21
maintenance_min_age_days = 17

[[agents]]
id = "main"

[agents.cortex]
branch_timeout_secs = 77
supervisor_kill_budget_per_tick = 3
association_max_per_pass = 55
maintenance_decay_rate = 0.33
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
        assert_eq!(config.defaults.cortex.maintenance_interval_secs, 1200);
        assert_eq!(config.defaults.cortex.maintenance_prune_threshold, 0.21);
        assert_eq!(config.defaults.cortex.maintenance_min_age_days, 17);

        assert_eq!(resolved.cortex.tick_interval_secs, 45);
        assert_eq!(resolved.cortex.branch_timeout_secs, 77);
        assert_eq!(resolved.cortex.detached_worker_timeout_retry_limit, 4);
        assert_eq!(resolved.cortex.supervisor_kill_budget_per_tick, 3);
        assert_eq!(resolved.cortex.bulletin_max_words, 1200);
        assert_eq!(resolved.cortex.maintenance_interval_secs, 1200);
        assert_eq!(resolved.cortex.maintenance_decay_rate, 0.33);
        assert_eq!(resolved.cortex.maintenance_prune_threshold, 0.21);
        assert_eq!(resolved.cortex.maintenance_min_age_days, 17);
        assert_eq!(resolved.cortex.maintenance_merge_similarity_threshold, 0.95);
        assert_eq!(resolved.cortex.association_max_per_pass, 55);
    }

    /// Card filing is on unless an operator says otherwise, and an agent can
    /// opt out on its own without the whole instance following.
    #[test]
    fn test_worker_task_create_defaults_on_and_is_overridable_per_agent() {
        let toml = r#"
[[agents]]
id = "main"

[[agents]]
id = "locked-down"

[agents.cortex]
worker_task_create = false
"#;
        let parsed: TomlConfig = toml::from_str(toml).expect("failed to parse test TOML");
        let config = Config::from_toml(parsed, PathBuf::from(".")).expect("failed to build Config");

        assert!(config.defaults.cortex.worker_task_create);
        assert!(
            config.agents[0]
                .resolve(&config.instance_dir, &config.defaults)
                .cortex
                .worker_task_create
        );
        assert!(
            !config.agents[1]
                .resolve(&config.instance_dir, &config.defaults)
                .cortex
                .worker_task_create
        );
    }

    #[test]
    fn test_cortex_maintenance_config_rejects_invalid_ranges() {
        let invalid_threshold = r#"
[defaults.cortex]
maintenance_prune_threshold = 1.2
"#;
        let parsed: TomlConfig =
            toml::from_str(invalid_threshold).expect("failed to parse invalid threshold TOML");
        assert!(
            Config::from_toml(parsed, PathBuf::from(".")).is_err(),
            "expected invalid maintenance_prune_threshold to be rejected"
        );

        let invalid_min_age = r#"
[defaults.cortex]
maintenance_min_age_days = -3
"#;
        let parsed: TomlConfig =
            toml::from_str(invalid_min_age).expect("failed to parse invalid min age TOML");
        assert!(
            Config::from_toml(parsed, PathBuf::from(".")).is_err(),
            "expected negative maintenance_min_age_days to be rejected"
        );

        let invalid_agent_override = r#"
[[agents]]
id = "main"

[agents.cortex]
maintenance_decay_rate = -0.1
"#;
        let parsed: TomlConfig =
            toml::from_str(invalid_agent_override).expect("failed to parse invalid agent TOML");
        assert!(
            Config::from_toml(parsed, PathBuf::from(".")).is_err(),
            "expected invalid agent maintenance_decay_rate to be rejected"
        );

        let invalid_interval = r#"
[defaults.cortex]
maintenance_interval_secs = 0
"#;
        let parsed: TomlConfig =
            toml::from_str(invalid_interval).expect("failed to parse invalid interval TOML");
        assert!(
            Config::from_toml(parsed, PathBuf::from(".")).is_err(),
            "expected maintenance_interval_secs = 0 to be rejected"
        );

        let invalid_merge_similarity_low = r#"
[defaults.cortex]
maintenance_merge_similarity_threshold = -0.1
"#;
        let parsed: TomlConfig = toml::from_str(invalid_merge_similarity_low)
            .expect("failed to parse invalid low merge similarity TOML");
        assert!(
            Config::from_toml(parsed, PathBuf::from(".")).is_err(),
            "expected low maintenance_merge_similarity_threshold to be rejected"
        );

        let invalid_merge_similarity_high = r#"
[defaults.cortex]
maintenance_merge_similarity_threshold = 1.1
"#;
        let parsed: TomlConfig = toml::from_str(invalid_merge_similarity_high)
            .expect("failed to parse invalid high merge similarity TOML");
        assert!(
            Config::from_toml(parsed, PathBuf::from(".")).is_err(),
            "expected high maintenance_merge_similarity_threshold to be rejected"
        );
    }

    #[test]
    fn test_participant_context_defaults_and_overrides_resolution() {
        let toml = r#"
[defaults.participant_context]
enabled = false
min_participants = 2
token_budget = 280
max_participants = 3

[[agents]]
id = "main"
"#;
        let parsed: TomlConfig = toml::from_str(toml).expect("failed to parse test TOML");
        let config = Config::from_toml(parsed, PathBuf::from(".")).expect("failed to build Config");

        assert!(!config.defaults.participant_context.enabled);
        assert_eq!(config.defaults.participant_context.min_participants, 2);
        assert_eq!(config.defaults.participant_context.token_budget, 280);
        assert_eq!(config.defaults.participant_context.max_participants, 3);
    }

    #[test]
    fn test_participant_context_rejects_impossible_bounds() {
        let toml = r#"
[defaults.participant_context]
min_participants = 4
max_participants = 3

[[agents]]
id = "main"
"#;
        let parsed: TomlConfig = toml::from_str(toml).expect("failed to parse test TOML");
        let error = Config::from_toml(parsed, PathBuf::from("."))
            .expect_err("expected invalid participant context bounds to fail");

        assert!(
            error
                .to_string()
                .contains("defaults.participant_context.max_participants (3) must be >= defaults.participant_context.min_participants (4)"),
            "unexpected error: {error}"
        );
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
    fn test_work_readiness_allows_old_synthesis() {
        // Knowledge synthesis is change-driven — staleness no longer blocks readiness.
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

        assert_eq!(readiness.bulletin_age_secs, Some(121));
        assert!(readiness.ready, "old synthesis should not block readiness");
        assert_eq!(readiness.reason, None);
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
            team_id: None,
            channel_ids: vec![],
            require_mention: false,
            dm_allowed_users: vec![],
            settings: None,
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
            team_id: None,
            channel_ids: vec![],
            require_mention: false,
            dm_allowed_users: vec![],
            settings: None,
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
            team_id: None,
            channel_ids: vec![],
            require_mention: false,
            dm_allowed_users: vec![],
            settings: None,
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
            team_id: None,
            channel_ids: vec![],
            require_mention: false,
            dm_allowed_users: vec![],
            settings: None,
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
            team_id: None,
            channel_ids: vec![],
            require_mention: false,
            dm_allowed_users: vec![],
            settings: None,
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
            team_id: None,
            channel_ids: vec![],
            require_mention: false,
            dm_allowed_users: vec![],
            settings: None,
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
            team_id: None,
            channel_ids: vec![],
            require_mention: false,
            dm_allowed_users: vec![],
            settings: None,
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
            signal: None,
            mattermost: None,
        };
        let bindings = vec![
            Binding {
                agent_id: "main".into(),
                channel: "telegram".into(),
                adapter: None,
                guild_id: None,
                workspace_id: None,
                chat_id: None,
                team_id: None,
                channel_ids: vec![],
                require_mention: false,
                dm_allowed_users: vec![],
                settings: None,
            },
            Binding {
                agent_id: "support-agent".into(),
                channel: "telegram".into(),
                adapter: Some("support".into()),
                guild_id: None,
                workspace_id: None,
                chat_id: None,
                team_id: None,
                channel_ids: vec![],
                require_mention: false,
                dm_allowed_users: vec![],
                settings: None,
            },
        ];
        let result = validate_named_messaging_adapters(&messaging, bindings, false)
            .expect("bindings should be resolvable");
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn validate_named_adapters_missing_instance_skipped() {
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
            signal: None,
            mattermost: None,
        };
        let bindings = vec![Binding {
            agent_id: "main".into(),
            channel: "telegram".into(),
            adapter: Some("nonexistent".into()),
            guild_id: None,
            workspace_id: None,
            chat_id: None,
            team_id: None,
            channel_ids: vec![],
            require_mention: false,
            dm_allowed_users: vec![],
            settings: None,
        }];
        let result = validate_named_messaging_adapters(&messaging, bindings, false)
            .expect("bindings should be resolvable");
        assert!(result.is_empty(), "unresolvable binding should be skipped");
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
            signal: None,
            mattermost: None,
        };
        let bindings = vec![Binding {
            agent_id: "main".into(),
            channel: "email".into(),
            adapter: Some("named".into()),
            guild_id: None,
            workspace_id: None,
            chat_id: None,
            team_id: None,
            channel_ids: vec![],
            require_mention: false,
            dm_allowed_users: vec![],
            settings: None,
        }];
        let result = validate_named_messaging_adapters(&messaging, bindings, false)
            .expect("bindings should be resolvable");
        assert!(
            result.is_empty(),
            "unsupported platform binding should be skipped"
        );
    }

    #[test]
    fn validate_binding_without_default_adapter_skipped() {
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
            signal: None,
            mattermost: None,
        };
        // Binding targets default adapter, but no default credentials exist
        let bindings = vec![Binding {
            agent_id: "main".into(),
            channel: "telegram".into(),
            adapter: None,
            guild_id: None,
            workspace_id: None,
            chat_id: None,
            team_id: None,
            channel_ids: vec![],
            require_mention: false,
            dm_allowed_users: vec![],
            settings: None,
        }];
        let result = validate_named_messaging_adapters(&messaging, bindings, false)
            .expect("bindings should be resolvable");
        assert!(
            result.is_empty(),
            "binding without default adapter should be skipped"
        );
    }

    #[test]
    fn validate_mixed_valid_and_invalid_bindings_filters_correctly() {
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
            signal: None,
            mattermost: None,
        };
        let bindings = vec![
            // Valid: default adapter with credentials
            Binding {
                agent_id: "agent-a".into(),
                channel: "telegram".into(),
                adapter: None,
                guild_id: None,
                workspace_id: None,
                chat_id: None,
                team_id: None,
                channel_ids: vec![],
                require_mention: false,
                dm_allowed_users: vec![],
                settings: None,
            },
            // Invalid: references a non-existent named adapter
            Binding {
                agent_id: "agent-b".into(),
                channel: "telegram".into(),
                adapter: Some("ghost".into()),
                guild_id: None,
                workspace_id: None,
                chat_id: None,
                team_id: None,
                channel_ids: vec![],
                require_mention: false,
                dm_allowed_users: vec![],
                settings: None,
            },
            // Valid: references an existing named adapter
            Binding {
                agent_id: "agent-c".into(),
                channel: "telegram".into(),
                adapter: Some("support".into()),
                guild_id: None,
                workspace_id: None,
                chat_id: None,
                team_id: None,
                channel_ids: vec![],
                require_mention: false,
                dm_allowed_users: vec![],
                settings: None,
            },
            // Invalid: no discord config at all
            Binding {
                agent_id: "agent-d".into(),
                channel: "discord".into(),
                adapter: None,
                guild_id: None,
                workspace_id: None,
                chat_id: None,
                team_id: None,
                channel_ids: vec![],
                require_mention: false,
                dm_allowed_users: vec![],
                settings: None,
            },
        ];
        let result = validate_named_messaging_adapters(&messaging, bindings, false)
            .expect("bindings should be resolvable");
        assert_eq!(
            result.len(),
            2,
            "only the two valid bindings should survive"
        );
        assert_eq!(result[0].agent_id, "agent-a");
        assert_eq!(result[1].agent_id, "agent-c");
    }

    #[test]
    fn validate_missing_messaging_config_skipped() {
        let messaging = MessagingConfig {
            discord: None,
            slack: None,
            telegram: None,
            email: None,
            webhook: None,
            twitch: None,
            signal: None,
            mattermost: None,
        };
        let bindings = vec![Binding {
            agent_id: "main".into(),
            channel: "telegram".into(),
            adapter: None,
            guild_id: None,
            workspace_id: None,
            chat_id: None,
            team_id: None,
            channel_ids: vec![],
            require_mention: false,
            dm_allowed_users: vec![],
            settings: None,
        }];
        let result = validate_named_messaging_adapters(&messaging, bindings, false)
            .expect("bindings should be resolvable");
        assert!(
            result.is_empty(),
            "binding with no messaging config should be skipped"
        );
    }

    #[test]
    fn validate_strict_mode_rejects_missing_messaging_config() {
        let messaging = MessagingConfig {
            discord: None,
            slack: None,
            telegram: None,
            email: None,
            webhook: None,
            twitch: None,
            signal: None,
            mattermost: None,
        };
        let bindings = vec![Binding {
            agent_id: "main".into(),
            channel: "telegram".into(),
            adapter: None,
            guild_id: None,
            workspace_id: None,
            chat_id: None,
            team_id: None,
            channel_ids: vec![],
            require_mention: false,
            dm_allowed_users: vec![],
            settings: None,
        }];
        let result = validate_named_messaging_adapters(&messaging, bindings, true);
        assert!(
            result.is_err(),
            "strict mode should reject unresolvable bindings"
        );
    }

    #[test]
    fn validate_disabled_instance_is_filtered_out() {
        let messaging = MessagingConfig {
            discord: None,
            slack: None,
            telegram: Some(TelegramConfig {
                enabled: true,
                token: "tok".into(),
                instances: vec![TelegramInstanceConfig {
                    name: "support".into(),
                    enabled: false,
                    token: "tok2".into(),
                    dm_allowed_users: vec![],
                }],
                dm_allowed_users: vec![],
            }),
            email: None,
            webhook: None,
            twitch: None,
            signal: None,
            mattermost: None,
        };
        let bindings = vec![Binding {
            agent_id: "main".into(),
            channel: "telegram".into(),
            adapter: Some("support".into()),
            guild_id: None,
            workspace_id: None,
            chat_id: None,
            team_id: None,
            channel_ids: vec![],
            require_mention: false,
            dm_allowed_users: vec![],
            settings: None,
        }];
        let result = validate_named_messaging_adapters(&messaging, bindings, false)
            .expect("bindings should be resolvable");
        assert!(
            result.is_empty(),
            "binding to disabled instance should be skipped"
        );
    }

    #[test]
    fn validate_disabled_platform_default_is_filtered_out() {
        let messaging = MessagingConfig {
            discord: None,
            slack: None,
            telegram: Some(TelegramConfig {
                enabled: false,
                token: "tok".into(),
                instances: vec![],
                dm_allowed_users: vec![],
            }),
            email: None,
            webhook: None,
            twitch: None,
            signal: None,
            mattermost: None,
        };
        let bindings = vec![Binding {
            agent_id: "main".into(),
            channel: "telegram".into(),
            adapter: None,
            guild_id: None,
            workspace_id: None,
            chat_id: None,
            team_id: None,
            channel_ids: vec![],
            require_mention: false,
            dm_allowed_users: vec![],
            settings: None,
        }];
        let result = validate_named_messaging_adapters(&messaging, bindings, false)
            .expect("bindings should be resolvable");
        assert!(
            result.is_empty(),
            "binding to disabled platform default should be skipped"
        );
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
    fn tool_use_enforcement_parses_and_resolves() {
        let toml = r#"
[defaults]
tool_use_enforcement = "always"

[[agents]]
id = "main"
tool_use_enforcement = ["gemini", "deepseek"]
"#;

        let parsed: TomlConfig = toml::from_str(toml).expect("failed to parse test TOML");
        let config = Config::from_toml(parsed, PathBuf::from(".")).expect("failed to build Config");

        assert_eq!(
            config.defaults.tool_use_enforcement,
            ToolUseEnforcement::Always
        );
        assert_eq!(
            config.agents[0].tool_use_enforcement,
            Some(ToolUseEnforcement::Custom(vec![
                "gemini".to_string(),
                "deepseek".to_string(),
            ]))
        );

        let resolved = config.resolve_agents();
        assert_eq!(
            resolved[0].tool_use_enforcement,
            ToolUseEnforcement::Custom(vec!["gemini".to_string(), "deepseek".to_string()])
        );
        assert!(
            resolved[0]
                .tool_use_enforcement
                .should_inject("google/gemini-2.5-pro")
        );
        assert!(
            !resolved[0]
                .tool_use_enforcement
                .should_inject("anthropic/claude-sonnet-4")
        );
    }
}
