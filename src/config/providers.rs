use super::toml_schema::TomlRoutingConfig;
use super::{ApiType, ProviderConfig};
use crate::llm::routing::RoutingConfig;

use std::collections::HashMap;

pub(super) const ANTHROPIC_PROVIDER_BASE_URL: &str = "https://api.anthropic.com";
pub(super) const OPENAI_PROVIDER_BASE_URL: &str = "https://api.openai.com";
pub(super) const OPENROUTER_PROVIDER_BASE_URL: &str = "https://openrouter.ai/api";
pub(super) const KILO_PROVIDER_BASE_URL: &str = "https://api.kilo.ai/api/gateway";
pub(super) const OLLAMA_PROVIDER_BASE_URL: &str = "http://localhost:11434";
pub(super) const OPENCODE_ZEN_PROVIDER_BASE_URL: &str = "https://opencode.ai/zen";
pub(super) const OPENCODE_GO_PROVIDER_BASE_URL: &str = "https://opencode.ai/zen/go";
pub(super) const MINIMAX_PROVIDER_BASE_URL: &str = "https://api.minimax.io/anthropic";
pub(super) const MINIMAX_CN_PROVIDER_BASE_URL: &str = "https://api.minimaxi.com/anthropic";
pub(super) const MOONSHOT_PROVIDER_BASE_URL: &str = "https://api.moonshot.ai";

pub(super) const ZHIPU_PROVIDER_BASE_URL: &str = "https://api.z.ai/api/paas/v4";
pub(super) const ZAI_CODING_PLAN_BASE_URL: &str = "https://api.z.ai/api/coding/paas/v4";
pub(super) const DEEPSEEK_PROVIDER_BASE_URL: &str = "https://api.deepseek.com";
pub(super) const GROQ_PROVIDER_BASE_URL: &str = "https://api.groq.com/openai";
pub(super) const TOGETHER_PROVIDER_BASE_URL: &str = "https://api.together.xyz";
pub(super) const XAI_PROVIDER_BASE_URL: &str = "https://api.x.ai";
pub(super) const MISTRAL_PROVIDER_BASE_URL: &str = "https://api.mistral.ai";
pub(super) const NVIDIA_PROVIDER_BASE_URL: &str = "https://integrate.api.nvidia.com";
pub(super) const FIREWORKS_PROVIDER_BASE_URL: &str = "https://api.fireworks.ai/inference";
pub(crate) const GEMINI_PROVIDER_BASE_URL: &str =
    "https://generativelanguage.googleapis.com/v1beta/openai";

/// App attribution headers sent with every OpenRouter API request.
/// See <https://openrouter.ai/docs/app-attribution>.
///
/// We send both legacy (`X-Title`) and new (`X-OpenRouter-Title`) header names
/// because (as of 2026-03-01) OpenRouter's backend still keys on the legacy names for populating
/// the app listing (title, etc.).
pub(super) fn openrouter_extra_headers() -> Vec<(String, String)> {
    vec![
        ("HTTP-Referer".into(), "https://spacebot.sh/".into()),
        ("X-Title".into(), "Spacebot".into()),
        ("X-OpenRouter-Title".into(), "Spacebot".into()),
        (
            "X-OpenRouter-Categories".into(),
            "cloud-agent,cli-agent".into(),
        ),
    ]
}

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
            extra_headers: vec![],
        },
        "openai" => ProviderConfig {
            api_type: ApiType::OpenAiCompletions,
            base_url: OPENAI_PROVIDER_BASE_URL.to_string(),
            api_key,
            name: None,
            use_bearer_auth: false,
            extra_headers: vec![],
        },
        "openrouter" => ProviderConfig {
            api_type: ApiType::OpenAiCompletions,
            base_url: OPENROUTER_PROVIDER_BASE_URL.to_string(),
            api_key,
            name: None,
            use_bearer_auth: false,
            extra_headers: openrouter_extra_headers(),
        },
        "kilo" => ProviderConfig {
            api_type: ApiType::KiloGateway,
            base_url: KILO_PROVIDER_BASE_URL.to_string(),
            api_key,
            name: Some("Kilo Gateway".to_string()),
            use_bearer_auth: false,
            extra_headers: vec![],
        },
        "zhipu" => ProviderConfig {
            api_type: ApiType::OpenAiChatCompletions,
            base_url: ZHIPU_PROVIDER_BASE_URL.to_string(),
            api_key,
            name: Some("Z.AI (GLM)".to_string()),
            use_bearer_auth: false,
            extra_headers: vec![],
        },
        "groq" => ProviderConfig {
            api_type: ApiType::OpenAiCompletions,
            base_url: GROQ_PROVIDER_BASE_URL.to_string(),
            api_key,
            name: None,
            use_bearer_auth: false,
            extra_headers: vec![],
        },
        "together" => ProviderConfig {
            api_type: ApiType::OpenAiCompletions,
            base_url: TOGETHER_PROVIDER_BASE_URL.to_string(),
            api_key,
            name: None,
            use_bearer_auth: false,
            extra_headers: vec![],
        },
        "fireworks" => ProviderConfig {
            api_type: ApiType::OpenAiCompletions,
            base_url: FIREWORKS_PROVIDER_BASE_URL.to_string(),
            api_key,
            name: None,
            use_bearer_auth: false,
            extra_headers: vec![],
        },
        "deepseek" => ProviderConfig {
            api_type: ApiType::OpenAiCompletions,
            base_url: DEEPSEEK_PROVIDER_BASE_URL.to_string(),
            api_key,
            name: None,
            use_bearer_auth: false,
            extra_headers: vec![],
        },
        "xai" => ProviderConfig {
            api_type: ApiType::OpenAiCompletions,
            base_url: XAI_PROVIDER_BASE_URL.to_string(),
            api_key,
            name: None,
            use_bearer_auth: false,
            extra_headers: vec![],
        },
        "mistral" => ProviderConfig {
            api_type: ApiType::OpenAiCompletions,
            base_url: MISTRAL_PROVIDER_BASE_URL.to_string(),
            api_key,
            name: None,
            use_bearer_auth: false,
            extra_headers: vec![],
        },
        "gemini" => ProviderConfig {
            api_type: ApiType::Gemini,
            base_url: GEMINI_PROVIDER_BASE_URL.to_string(),
            api_key,
            name: None,
            use_bearer_auth: false,
            extra_headers: vec![],
        },
        "ollama" => ProviderConfig {
            api_type: ApiType::OpenAiCompletions,
            base_url: api_key,
            api_key: String::new(),
            name: None,
            use_bearer_auth: false,
            extra_headers: vec![],
        },
        "opencode-zen" => ProviderConfig {
            api_type: ApiType::OpenAiCompletions,
            base_url: OPENCODE_ZEN_PROVIDER_BASE_URL.to_string(),
            api_key,
            name: None,
            use_bearer_auth: false,
            extra_headers: vec![],
        },
        "opencode-go" => ProviderConfig {
            api_type: ApiType::OpenAiCompletions,
            base_url: OPENCODE_GO_PROVIDER_BASE_URL.to_string(),
            api_key,
            name: None,
            use_bearer_auth: false,
            extra_headers: vec![],
        },
        "nvidia" => ProviderConfig {
            api_type: ApiType::OpenAiCompletions,
            base_url: NVIDIA_PROVIDER_BASE_URL.to_string(),
            api_key,
            name: None,
            use_bearer_auth: false,
            extra_headers: vec![],
        },
        "minimax" => ProviderConfig {
            api_type: ApiType::Anthropic,
            base_url: MINIMAX_PROVIDER_BASE_URL.to_string(),
            api_key,
            name: None,
            use_bearer_auth: false,
            extra_headers: vec![],
        },
        "minimax-cn" => ProviderConfig {
            api_type: ApiType::Anthropic,
            base_url: MINIMAX_CN_PROVIDER_BASE_URL.to_string(),
            api_key,
            name: None,
            use_bearer_auth: false,
            extra_headers: vec![],
        },
        "moonshot" => ProviderConfig {
            api_type: ApiType::OpenAiCompletions,
            base_url: MOONSHOT_PROVIDER_BASE_URL.to_string(),
            api_key,
            name: None,
            use_bearer_auth: false,
            extra_headers: vec![],
        },
        "zai-coding-plan" => ProviderConfig {
            api_type: ApiType::OpenAiChatCompletions,
            base_url: ZAI_CODING_PLAN_BASE_URL.to_string(),
            api_key,
            name: Some("Z.AI Coding Plan".to_string()),
            use_bearer_auth: false,
            extra_headers: vec![],
        },
        _ => return None,
    })
}

pub(super) fn add_shorthand_provider(
    providers: &mut HashMap<String, ProviderConfig>,
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
                extra_headers: vec![],
            });
    }
}

/// When `[defaults.routing]` is absent from the config file, pick routing
/// defaults based on which provider the user actually has configured.  This
/// avoids the common pitfall where a user sets up OpenRouter (or another
/// non-Anthropic provider) but new agents still default to
/// `anthropic/claude-sonnet-4` and every LLM call fails.
///
/// Provider priority: first-party Anthropic first, then major gateways,
/// then smaller providers. If the user only has one provider configured
/// this always picks the right one.
pub(super) fn infer_routing_from_providers(
    providers: &HashMap<String, ProviderConfig>,
) -> Option<RoutingConfig> {
    const PRIORITY: &[&str] = &[
        "anthropic",
        "openrouter",
        "kilo",
        "openai",
        "openai-chatgpt",
        "deepseek",
        "gemini",
        "xai",
        "groq",
        "together",
        "fireworks",
        "mistral",
        "zhipu",
        "ollama",
        "opencode-zen",
        "opencode-go",
        "nvidia",
        "minimax",
        "minimax-cn",
        "moonshot",
        "zai-coding-plan",
    ];

    for &name in PRIORITY {
        if providers.contains_key(name) {
            return Some(crate::llm::routing::defaults_for_provider(name));
        }
    }

    // Fall back to the first provider in the map (covers custom providers).
    providers
        .keys()
        .next()
        .map(|name| crate::llm::routing::defaults_for_provider(name))
}

/// Resolve a TomlRoutingConfig against a base RoutingConfig.
pub(super) fn resolve_routing(
    toml: Option<TomlRoutingConfig>,
    base: &RoutingConfig,
) -> RoutingConfig {
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
