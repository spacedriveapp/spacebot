//! LLM manager for provider credentials and HTTP client.
//!
//! The manager is intentionally simple — it holds API keys, an HTTP client,
//! and shared rate limit state. Routing decisions (which model for which
//! process) live on the agent's RoutingConfig, not here.

use crate::config::LlmConfig;
use crate::error::{LlmError, Result};
use anyhow::Context as _;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::RwLock;

// Default API endpoints per provider (used when no base_url is configured).
const DEFAULT_ANTHROPIC_BASE_URL: &str = "https://api.anthropic.com/v1/messages";
const DEFAULT_OPENAI_BASE_URL: &str = "https://api.openai.com/v1/chat/completions";
const DEFAULT_OPENROUTER_BASE_URL: &str = "https://openrouter.ai/api/v1/chat/completions";
const DEFAULT_ZHIPU_BASE_URL: &str = "https://api.z.ai/api/paas/v4/chat/completions";
const DEFAULT_GROQ_BASE_URL: &str = "https://api.groq.com/openai/v1/chat/completions";
const DEFAULT_TOGETHER_BASE_URL: &str = "https://api.together.xyz/v1/chat/completions";
const DEFAULT_FIREWORKS_BASE_URL: &str = "https://api.fireworks.ai/inference/v1/chat/completions";
const DEFAULT_DEEPSEEK_BASE_URL: &str = "https://api.deepseek.com/v1/chat/completions";
const DEFAULT_XAI_BASE_URL: &str = "https://api.x.ai/v1/chat/completions";
const DEFAULT_MISTRAL_BASE_URL: &str = "https://api.mistral.ai/v1/chat/completions";
const DEFAULT_OPENCODE_ZEN_BASE_URL: &str = "https://opencode.ai/zen/v1/chat/completions";

/// Manages LLM provider clients and tracks rate limit state.
pub struct LlmManager {
    config: LlmConfig,
    http_client: reqwest::Client,
    /// Models currently in rate limit cooldown, with the time they were limited.
    rate_limited: Arc<RwLock<HashMap<String, Instant>>>,
}

impl LlmManager {
    /// Create a new LLM manager with the given configuration.
    pub async fn new(config: LlmConfig) -> Result<Self> {
        let http_client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(120))
            .build()
            .with_context(|| "failed to build HTTP client")?;

        Ok(Self {
            config,
            http_client,
            rate_limited: Arc::new(RwLock::new(HashMap::new())),
        })
    }

    /// Get the appropriate API key for a provider.
    pub fn get_api_key(&self, provider: &str) -> Result<String> {
        match provider {
            "anthropic" => self.config.anthropic_key.clone()
                .ok_or_else(|| LlmError::MissingProviderKey("anthropic".into()).into()),
            "openai" => self.config.openai_key.clone()
                .ok_or_else(|| LlmError::MissingProviderKey("openai".into()).into()),
            "openrouter" => self.config.openrouter_key.clone()
                .ok_or_else(|| LlmError::MissingProviderKey("openrouter".into()).into()),
            "zhipu" => self.config.zhipu_key.clone()
                .ok_or_else(|| LlmError::MissingProviderKey("zhipu".into()).into()),
            "groq" => self.config.groq_key.clone()
                .ok_or_else(|| LlmError::MissingProviderKey("groq".into()).into()),
            "together" => self.config.together_key.clone()
                .ok_or_else(|| LlmError::MissingProviderKey("together".into()).into()),
            "fireworks" => self.config.fireworks_key.clone()
                .ok_or_else(|| LlmError::MissingProviderKey("fireworks".into()).into()),
            "deepseek" => self.config.deepseek_key.clone()
                .ok_or_else(|| LlmError::MissingProviderKey("deepseek".into()).into()),
            "xai" => self.config.xai_key.clone()
                .ok_or_else(|| LlmError::MissingProviderKey("xai".into()).into()),
            "mistral" => self.config.mistral_key.clone()
                .ok_or_else(|| LlmError::MissingProviderKey("mistral".into()).into()),
            "opencode-zen" => self.config.opencode_zen_key.clone()
                .ok_or_else(|| LlmError::MissingProviderKey("opencode-zen".into()).into()),
            _ => Err(LlmError::UnknownProvider(provider.into()).into()),
        }
    }

    /// Get the base URL for a provider, falling back to the default.
    ///
    /// Panics if `provider` is not a known provider name — callers must
    /// validate the provider string before reaching this point.
    pub fn get_base_url(&self, provider: &str) -> &str {
        match provider {
            "anthropic" => self.config.anthropic_base_url.as_deref()
                .unwrap_or(DEFAULT_ANTHROPIC_BASE_URL),
            "openai" => self.config.openai_base_url.as_deref()
                .unwrap_or(DEFAULT_OPENAI_BASE_URL),
            "openrouter" => self.config.openrouter_base_url.as_deref()
                .unwrap_or(DEFAULT_OPENROUTER_BASE_URL),
            "zhipu" => self.config.zhipu_base_url.as_deref()
                .unwrap_or(DEFAULT_ZHIPU_BASE_URL),
            "groq" => self.config.groq_base_url.as_deref()
                .unwrap_or(DEFAULT_GROQ_BASE_URL),
            "together" => self.config.together_base_url.as_deref()
                .unwrap_or(DEFAULT_TOGETHER_BASE_URL),
            "fireworks" => self.config.fireworks_base_url.as_deref()
                .unwrap_or(DEFAULT_FIREWORKS_BASE_URL),
            "deepseek" => self.config.deepseek_base_url.as_deref()
                .unwrap_or(DEFAULT_DEEPSEEK_BASE_URL),
            "xai" => self.config.xai_base_url.as_deref()
                .unwrap_or(DEFAULT_XAI_BASE_URL),
            "mistral" => self.config.mistral_base_url.as_deref()
                .unwrap_or(DEFAULT_MISTRAL_BASE_URL),
            "opencode-zen" => self.config.opencode_zen_base_url.as_deref()
                .unwrap_or(DEFAULT_OPENCODE_ZEN_BASE_URL),
            _ => unreachable!("unknown provider: {provider}"),
        }
    }

    /// Get the HTTP client.
    pub fn http_client(&self) -> &reqwest::Client {
        &self.http_client
    }

    /// Resolve a model name to provider and model components.
    /// Format: "provider/model-name" or just "model-name" (defaults to anthropic).
    pub fn resolve_model(&self, model_name: &str) -> Result<(String, String)> {
        if let Some((provider, model)) = model_name.split_once('/') {
            Ok((provider.to_string(), model.to_string()))
        } else {
            Ok(("anthropic".into(), model_name.into()))
        }
    }

    /// Record that a model hit a rate limit.
    pub async fn record_rate_limit(&self, model_name: &str) {
        self.rate_limited.write().await
            .insert(model_name.to_string(), Instant::now());
        tracing::warn!(model = %model_name, "model rate limited, entering cooldown");
    }

    /// Check if a model is currently in rate limit cooldown.
    pub async fn is_rate_limited(&self, model_name: &str, cooldown_secs: u64) -> bool {
        let map = self.rate_limited.read().await;
        if let Some(limited_at) = map.get(model_name) {
            limited_at.elapsed().as_secs() < cooldown_secs
        } else {
            false
        }
    }

    /// Clean up expired rate limit entries.
    pub async fn cleanup_rate_limits(&self, cooldown_secs: u64) {
        self.rate_limited.write().await
            .retain(|_, limited_at| limited_at.elapsed().as_secs() < cooldown_secs);
    }
}
