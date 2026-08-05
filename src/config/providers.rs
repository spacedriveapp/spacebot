use super::ProviderConfig;
use super::toml_schema::TomlRoutingConfig;
use crate::llm::routing::RoutingConfig;

use std::collections::HashMap;

/// Default endpoint for the native Anthropic Messages API.
///
/// This is the only base URL Spacebot still hardcodes. Every other provider is
/// an `openai_compatible` entry whose `base_url` the user supplies, because
/// baking in 20 vendor URLs meant 20 things to keep current and 20 reasons to
/// touch this file.
pub(crate) const ANTHROPIC_PROVIDER_BASE_URL: &str = "https://api.anthropic.com";

/// Default endpoint for a local LiteLLM proxy.
///
/// LiteLLM is the recommended way to reach anything that is not Anthropic, so
/// `LITELLM_API_KEY` alone is enough to bootstrap a working config.
pub(super) const LITELLM_PROVIDER_BASE_URL: &str = "http://localhost:4000/v1";

/// When `[defaults.routing]` is absent from the config file, pick routing
/// defaults based on which provider the user actually has configured. This
/// avoids the pitfall where a user sets up a gateway but new agents still
/// default to `anthropic/claude-sonnet-4` and every LLM call fails.
pub(super) fn infer_routing_from_providers(
    providers: &HashMap<String, ProviderConfig>,
) -> Option<RoutingConfig> {
    // Anthropic first (it has real model-name defaults), then LiteLLM, then
    // whatever single provider the user defined.
    const PRIORITY: &[&str] = &["anthropic", "litellm"];

    for &name in PRIORITY {
        if providers.contains_key(name) {
            return Some(crate::llm::routing::defaults_for_provider(name));
        }
    }

    // Fall back to the first provider in the map (covers custom providers).
    // HashMap iteration order is arbitrary, so only trust it when there is
    // exactly one candidate; otherwise routing would flip between boots.
    if providers.len() != 1 {
        return None;
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ApiType;

    fn provider() -> ProviderConfig {
        ProviderConfig {
            api_type: ApiType::OpenAiCompatible,
            base_url: "http://localhost:4000/v1".to_string(),
            api_key: "sk-test".to_string(),
            name: None,
            use_bearer_auth: false,
            extra_headers: vec![],
        }
    }

    #[test]
    fn infer_routing_from_litellm_only_is_empty() {
        // LiteLLM's model catalog is operator-defined, so inference must not
        // fall back to the anthropic default — every route stays empty and
        // completion fails with an explicit "no model configured" error.
        let mut providers = HashMap::new();
        providers.insert("litellm".to_string(), provider());

        let routing = infer_routing_from_providers(&providers).expect("litellm should infer");
        assert!(routing.channel.is_empty());
        assert!(routing.branch.is_empty());
        assert!(routing.worker.is_empty());
        assert!(routing.compactor.is_empty());
        assert!(routing.cortex.is_empty());
    }

    #[test]
    fn infer_routing_prefers_anthropic_defaults() {
        let mut providers = HashMap::new();
        providers.insert("litellm".to_string(), provider());
        providers.insert(
            "anthropic".to_string(),
            ProviderConfig {
                api_type: ApiType::Anthropic,
                ..provider()
            },
        );

        let routing = infer_routing_from_providers(&providers).expect("anthropic should infer");
        assert_eq!(routing.channel, "anthropic/claude-sonnet-4");
    }
}
