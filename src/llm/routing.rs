//! Model routing configuration and resolution.

use crate::ProcessType;
use std::collections::HashMap;

/// Model routing configuration. Lives on the agent config (via defaults).
/// Determines which LLM model each process type uses, with task-type
/// overrides for workers/branches and fallback chains for resilience.
#[derive(Debug, Clone)]
pub struct RoutingConfig {
    /// Model per process type.
    pub channel: String,
    pub branch: String,
    pub worker: String,
    pub compactor: String,
    pub cortex: String,

    /// Task-type overrides (e.g. "coding" → "anthropic/claude-sonnet-4").
    /// Applied to workers and branches when a task_type is specified at spawn.
    pub task_overrides: HashMap<String, String>,

    /// Fallback chains per model. When a model fails with a retriable error,
    /// try the next model in its chain.
    pub fallbacks: HashMap<String, Vec<String>>,

    /// How long to deprioritize a rate-limited model (seconds).
    pub rate_limit_cooldown_secs: u64,
}

impl Default for RoutingConfig {
    fn default() -> Self {
        Self::for_model("anthropic/claude-sonnet-4-20250514".into())
    }
}

impl RoutingConfig {
    /// Create a routing config that uses a single model for all process types.
    fn for_model(model: String) -> Self {
        Self {
            channel: model.clone(),
            branch: model.clone(),
            worker: model.clone(),
            compactor: model.clone(),
            cortex: model,
            task_overrides: HashMap::new(),
            fallbacks: HashMap::new(),
            rate_limit_cooldown_secs: 60,
        }
    }
}

impl RoutingConfig {
    /// Resolve the model name for a process type and optional task type.
    pub fn resolve(&self, process_type: ProcessType, task_type: Option<&str>) -> &str {
        // Check task-type override first (only for workers and branches)
        if let Some(task) = task_type {
            if matches!(process_type, ProcessType::Worker | ProcessType::Branch) {
                if let Some(override_model) = self.task_overrides.get(task) {
                    return override_model;
                }
            }
        }

        match process_type {
            ProcessType::Channel => &self.channel,
            ProcessType::Branch => &self.branch,
            ProcessType::Worker => &self.worker,
            ProcessType::Compactor => &self.compactor,
            ProcessType::Cortex => &self.cortex,
        }
    }

    /// Get the fallback chain for a model, if any.
    pub fn get_fallbacks(&self, model_name: &str) -> &[String] {
        self.fallbacks
            .get(model_name)
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }
}

/// Whether an HTTP status code should trigger a fallback to the next model.
pub fn is_retriable_status(status: u16) -> bool {
    matches!(status, 429 | 502 | 503 | 504)
}

/// Whether a completion error message indicates a retriable failure.
pub fn is_retriable_error(error_message: &str) -> bool {
    let lower = error_message.to_lowercase();
    // Rate limits and server errors
    lower.contains("429")
        || lower.contains("502")
        || lower.contains("503")
        || lower.contains("504")
        || lower.contains("rate limit")
        || lower.contains("overloaded")
        || lower.contains("timeout")
        || lower.contains("connection")
        // Empty/malformed responses are transient provider issues
        || lower.contains("empty response")
        || lower.contains("failed to read response body")
        || lower.contains("error decoding response body")
}

/// Whether a completion error indicates context window overflow.
///
/// Providers return 400 with various phrasings when the request exceeds
/// the model's context limit. Checking for these lets workers compact
/// and retry instead of dying.
pub fn is_context_overflow_error(error_message: &str) -> bool {
    let lower = error_message.to_lowercase();
    lower.contains("context length")
        || lower.contains("maximum context")
        || lower.contains("token limit")
        || lower.contains("too many tokens")
        || lower.contains("request too large")
        || lower.contains("content_too_large")
        || lower.contains("max_tokens")
        || (lower.contains("maximum") && lower.contains("tokens"))
}

/// Returns routing defaults appropriate for a given provider.
///
/// When a user sets up OpenRouter but routing still points to `anthropic/...`,
/// every LLM call fails because there's no Anthropic key. This function gives
/// each provider sane defaults so things work out of the box.
pub fn defaults_for_provider(provider: &str) -> RoutingConfig {
    match provider {
        "anthropic" => RoutingConfig::for_model("anthropic/claude-sonnet-4-20250514".into()),
        "openrouter" => {
            RoutingConfig::for_model("openrouter/anthropic/claude-sonnet-4-20250514".into())
        }
        "openai" => RoutingConfig::for_model("openai/gpt-4.1".into()),
        "zhipu" => RoutingConfig::for_model("zhipu/glm-4-plus".into()),
        "groq" => RoutingConfig::for_model("groq/llama-3.3-70b-versatile".into()),
        "together" => RoutingConfig::for_model(
            "together/meta-llama/Meta-Llama-3.1-405B-Instruct-Turbo".into(),
        ),
        "fireworks" => RoutingConfig::for_model(
            "fireworks/accounts/fireworks/models/llama-v3p3-70b-instruct".into(),
        ),
        "deepseek" => RoutingConfig::for_model("deepseek/deepseek-chat".into()),
        "xai" => RoutingConfig::for_model("xai/grok-2-latest".into()),
        "mistral" => RoutingConfig::for_model("mistral/mistral-large-latest".into()),
        "nvidia" => RoutingConfig::for_model("nvidia/meta/llama-3.1-405b-instruct".into()),
        "opencode-zen" => RoutingConfig::for_model("opencode-zen/kimi-k2.5".into()),
        _ => RoutingConfig::default(),
    }
}

/// Maps a provider ID to the prefix used in model routing strings.
pub fn provider_to_prefix(provider: &str) -> &str {
    match provider {
        "openrouter" => "openrouter/",
        "openai" => "openai/",
        "anthropic" => "anthropic/",
        "zhipu" => "zhipu/",
        "groq" => "groq/",
        "together" => "together/",
        "fireworks" => "fireworks/",
        "deepseek" => "deepseek/",
        "xai" => "xai/",
        "mistral" => "mistral/",
        "nvidia" => "nvidia/",
        "opencode-zen" => "opencode-zen/",
        _ => "",
    }
}

/// Extracts the provider from a model routing string.
pub fn provider_from_model(model: &str) -> &str {
    if let Some((provider, _)) = model.split_once('/') {
        provider
    } else {
        "anthropic"
    }
}

/// Max number of fallback models to try before giving up.
pub const MAX_FALLBACK_ATTEMPTS: usize = 3;

/// Max retries per model (primary or fallback) on retriable errors.
pub const MAX_RETRIES_PER_MODEL: usize = 3;

/// Base delay for exponential backoff between retries (milliseconds).
pub const RETRY_BASE_DELAY_MS: u64 = 500;

/// Whether an error indicates an actual rate limit (429) vs other transient failures.
/// Only rate-limit errors should trigger cooldown — timeouts and 5xx errors are
/// momentary and shouldn't lock out a model for the full cooldown period.
pub fn is_rate_limit_error(error_message: &str) -> bool {
    let lower = error_message.to_lowercase();
    lower.contains("429") || lower.contains("rate limit")
}
