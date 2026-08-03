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
    pub voice: String,

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
            voice: String::new(),
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
        if let Some(task) = task_type
            && matches!(process_type, ProcessType::Worker | ProcessType::Branch)
            && let Some(override_model) = self.task_overrides.get(task)
        {
            return override_model;
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
        if self.channel == model_name {
            return &self.channel_thinking_effort;
        }
        if self.branch == model_name {
            return &self.branch_thinking_effort;
        }
        if self.worker == model_name {
            return &self.worker_thinking_effort;
        }
        if self.compactor == model_name {
            return &self.compactor_thinking_effort;
        }
        if self.cortex == model_name {
            return &self.cortex_thinking_effort;
        }
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
        || lower.contains("500")
        || lower.contains("502")
        || lower.contains("503")
        || lower.contains("504")
        || lower.contains("rate limit")
        || lower.contains("overloaded")
        || lower.contains("timeout")
        || lower.contains("connection")
        || lower.contains("error sending request")
        // Generic server errors (OpenRouter wraps upstream 500s in various
        // phrasings like "The server had an error while processing your request")
        || lower.contains("server error")
        || lower.contains("server had an error")
        || lower.contains("internal error")
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
/// Only `anthropic` has model names we can name with confidence — it is the one
/// provider whose catalog Spacebot targets natively. For every other provider
/// the model catalog is whatever the operator configured upstream (LiteLLM
/// aliases, vLLM served-model names, Ollama tags), so guessing a model string
/// would produce a config that looks right and 404s on first use. Those get the
/// standard defaults, which the operator is expected to override.
pub fn defaults_for_provider(provider: &str) -> RoutingConfig {
    match provider {
        "anthropic" => RoutingConfig::for_model("anthropic/claude-sonnet-4".into()),
        _ => RoutingConfig::default(),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_retriable_error_catches_network_failures() {
        // DNS/connection failures from reqwest
        assert!(is_retriable_error(
            "error sending request for url (https://api.z.ai/api/anthropic/v1/messages)"
        ));
        assert!(is_retriable_error(
            "error sending request: connection refused"
        ));
        assert!(is_retriable_error("ERROR SENDING REQUEST: timeout"));
    }

    #[test]
    fn is_retriable_error_catches_http_errors() {
        // 5xx server errors
        assert!(is_retriable_error("502 Bad Gateway"));
        assert!(is_retriable_error("503 Service Unavailable"));
        assert!(is_retriable_error("504 Gateway Timeout"));
        assert!(is_retriable_error("500 Internal Server Error"));
        // Rate limiting
        assert!(is_retriable_error("429 Too Many Requests"));
        assert!(is_retriable_error("rate limit exceeded"));
    }

    #[test]
    fn is_retriable_error_catches_timeout_and_connection_errors() {
        assert!(is_retriable_error("connection timeout"));
        assert!(is_retriable_error("connection reset by peer"));
        assert!(is_retriable_error("timeout while waiting for response"));
    }

    #[test]
    fn is_retriable_error_catches_server_error_phrases() {
        assert!(is_retriable_error(
            "The server had an error while processing your request"
        ));
        assert!(is_retriable_error("internal error"));
        assert!(is_retriable_error("server error"));
        assert!(is_retriable_error("overloaded"));
    }

    #[test]
    fn is_retriable_error_catches_malformed_responses() {
        assert!(is_retriable_error("empty response"));
        assert!(is_retriable_error("failed to read response body"));
        assert!(is_retriable_error("error decoding response body"));
    }

    #[test]
    fn is_retriable_error_rejects_non_retriable_errors() {
        // Auth errors should not be retriable
        assert!(!is_retriable_error("Invalid API key"));
        assert!(!is_retriable_error("401 Unauthorized"));
        assert!(!is_retriable_error("403 Forbidden"));
        // Client errors should not be retriable
        assert!(!is_retriable_error("400 Bad Request"));
        assert!(!is_retriable_error("404 Not Found"));
        // Other errors should not be retriable
        assert!(!is_retriable_error("unexpected EOF"));
        assert!(!is_retriable_error("parse error"));
    }

    #[test]
    fn is_rate_limit_error_detection() {
        assert!(is_rate_limit_error("429 Too Many Requests"));
        assert!(is_rate_limit_error("rate limit exceeded"));
        assert!(is_rate_limit_error("RATE LIMIT: too many requests"));
        // Other transient errors should not be rate limited
        assert!(!is_rate_limit_error("503 Service Unavailable"));
        assert!(!is_rate_limit_error("timeout"));
    }
}
