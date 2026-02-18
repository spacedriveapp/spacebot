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

    pub channel_thinking_effort: String,
    pub branch_thinking_effort: String,
    pub worker_thinking_effort: String,
    pub compactor_thinking_effort: String,
    pub cortex_thinking_effort: String,
}

impl Default for RoutingConfig {
    fn default() -> Self {
        Self::for_model("anthropic/claude-sonnet-4".into())
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
            channel_thinking_effort: "auto".into(),
            branch_thinking_effort: "auto".into(),
            worker_thinking_effort: "auto".into(),
            compactor_thinking_effort: "auto".into(),
            cortex_thinking_effort: "auto".into(),
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

    pub fn thinking_effort_for_model(&self, model_name: &str) -> &str {
        if self.channel == model_name { return &self.channel_thinking_effort; }
        if self.branch == model_name { return &self.branch_thinking_effort; }
        if self.worker == model_name { return &self.worker_thinking_effort; }
        if self.compactor == model_name { return &self.compactor_thinking_effort; }
        if self.cortex == model_name { return &self.cortex_thinking_effort; }
        "auto"
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
        "anthropic" => RoutingConfig::for_model("anthropic/claude-sonnet-4".into()),
        "openrouter" => {
            let channel: String = "openrouter/anthropic/claude-sonnet-4-20250514".into();
            let worker: String = "openrouter/anthropic/claude-haiku-4.5-20250514".into();
            RoutingConfig {
                channel: "openrouter/anthropic/claude-sonnet-4-20250514".into(),
                branch: "openrouter/anthropic/claude-sonnet-4-20250514".into(),
                worker: "openrouter/anthropic/claude-haiku-4.5-20250514".into(),
                compactor: "openrouter/anthropic/claude-haiku-4.5-20250514".into(),
                cortex: "openrouter/anthropic/claude-haiku-4.5-20250514".into(),
                task_overrides: HashMap::from([("coding".into(), channel.clone())]),
                fallbacks: HashMap::from([(channel, vec![worker])]),
                rate_limit_cooldown_secs: 60,
                ..RoutingConfig::default()
            }
        }
        "openai" => {
            let channel: String = "openai/gpt-4.1".into();
            let worker: String = "openai/gpt-4.1-mini".into();
            RoutingConfig {
                channel: channel.clone(),
                branch: channel.clone(),
                worker: worker.clone(),
                compactor: worker.clone(),
                cortex: worker.clone(),
                task_overrides: HashMap::from([("coding".into(), channel.clone())]),
                fallbacks: HashMap::from([(channel, vec![worker])]),
                rate_limit_cooldown_secs: 60,
                ..RoutingConfig::default()
            }
        }
        "zhipu" => {
            let channel: String = "zhipu/glm-4-plus".into();
            let worker: String = "zhipu/glm-4-flash".into();
            RoutingConfig {
                channel: channel.clone(),
                branch: channel.clone(),
                worker: worker.clone(),
                compactor: worker.clone(),
                cortex: worker.clone(),
                task_overrides: HashMap::from([("coding".into(), channel.clone())]),
                fallbacks: HashMap::from([(channel, vec![worker])]),
                rate_limit_cooldown_secs: 60,
                ..RoutingConfig::default()
            }
        }
        "groq" => {
            let channel: String = "groq/llama-3.3-70b-versatile".into();
            let worker: String = "groq/llama-3.3-70b-specdec".into();
            RoutingConfig {
                channel: channel.clone(),
                branch: channel.clone(),
                worker: worker.clone(),
                compactor: worker.clone(),
                cortex: worker.clone(),
                task_overrides: HashMap::from([("coding".into(), channel.clone())]),
                fallbacks: HashMap::from([(channel, vec![worker])]),
                rate_limit_cooldown_secs: 60,
                ..RoutingConfig::default()
            }
        }
        "together" => {
            let channel: String = "together/meta-llama/Meta-Llama-3.1-405B-Instruct-Turbo".into();
            let worker: String = "together/meta-llama/Meta-Llama-3.1-70B-Instruct-Turbo".into();
            RoutingConfig {
                channel: channel.clone(),
                branch: channel.clone(),
                worker: worker.clone(),
                compactor: worker.clone(),
                cortex: worker.clone(),
                task_overrides: HashMap::from([("coding".into(), channel.clone())]),
                fallbacks: HashMap::from([(channel, vec![worker])]),
                rate_limit_cooldown_secs: 60,
                ..RoutingConfig::default()
            }
        }
        "fireworks" => {
            let channel: String =
                "fireworks/accounts/fireworks/models/llama-v3p3-70b-instruct".into();
            let worker: String =
                "fireworks/accounts/fireworks/models/llama-v3p1-8b-instruct".into();
            RoutingConfig {
                channel: channel.clone(),
                branch: channel.clone(),
                worker: worker.clone(),
                compactor: worker.clone(),
                cortex: worker.clone(),
                task_overrides: HashMap::from([("coding".into(), channel.clone())]),
                fallbacks: HashMap::from([(channel, vec![worker])]),
                rate_limit_cooldown_secs: 60,
                ..RoutingConfig::default()
            }
        }
        "deepseek" => {
            let channel: String = "deepseek/deepseek-chat".into();
            let worker: String = "deepseek/deepseek-chat".into();
            RoutingConfig {
                channel: channel.clone(),
                branch: channel.clone(),
                worker: worker.clone(),
                compactor: worker.clone(),
                cortex: worker.clone(),
                task_overrides: HashMap::from([("coding".into(), channel.clone())]),
                fallbacks: HashMap::new(),
                rate_limit_cooldown_secs: 60,
                ..RoutingConfig::default()
            }
        }
        "xai" => {
            let channel: String = "xai/grok-2-latest".into();
            let worker: String = "xai/grok-2-latest".into();
            RoutingConfig {
                channel: channel.clone(),
                branch: channel.clone(),
                worker: worker.clone(),
                compactor: worker.clone(),
                cortex: worker.clone(),
                task_overrides: HashMap::from([("coding".into(), channel.clone())]),
                fallbacks: HashMap::new(),
                rate_limit_cooldown_secs: 60,
                ..RoutingConfig::default()
            }
        }
        "mistral" => {
            let channel: String = "mistral/mistral-large-latest".into();
            let worker: String = "mistral/mistral-small-latest".into();
            RoutingConfig {
                channel: channel.clone(),
                branch: channel.clone(),
                worker: worker.clone(),
                compactor: worker.clone(),
                cortex: worker.clone(),
                task_overrides: HashMap::from([("coding".into(), channel.clone())]),
                fallbacks: HashMap::from([(channel, vec![worker])]),
                rate_limit_cooldown_secs: 60,
                ..RoutingConfig::default()
            }
        }
        "opencode-zen" => {
            let channel: String = "opencode-zen/kimi-k2.5".into();
            let worker: String = "opencode-zen/kimi-k2.5".into();
            RoutingConfig {
                channel: channel.clone(),
                branch: channel.clone(),
                worker: worker.clone(),
                compactor: worker.clone(),
                cortex: worker.clone(),
                task_overrides: HashMap::from([("coding".into(), channel.clone())]),
                fallbacks: HashMap::new(),
                rate_limit_cooldown_secs: 60,
                ..RoutingConfig::default()
            }
        }
        "nvidia" => RoutingConfig::for_model("nvidia/meta/llama-3.1-405b-instruct".into()),
        "minimax" => RoutingConfig::for_model("minimax/MiniMax-M1-80k".into()),
        "moonshot" => RoutingConfig::for_model("moonshot/kimi-k2.5".into()),
        "zai-coding-plan" => RoutingConfig::for_model("zai-coding-plan/glm-5".into()),
        // Unknown — use the standard defaults
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
        "minimax" => "minimax/",
        "moonshot" => "moonshot/",
        "zai-coding-plan" => "zai-coding-plan/",
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
