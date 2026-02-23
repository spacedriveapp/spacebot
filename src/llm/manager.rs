//! LLM manager for provider credentials and HTTP client.
//!
//! The manager is intentionally simple — it holds API keys, an HTTP client,
//! and shared rate limit state. Routing decisions (which model for which
//! process) live on the agent's RoutingConfig, not here.
//!
//! API keys are hot-reloadable via ArcSwap. The file watcher calls
//! `reload_config()` when config.toml changes, and all subsequent
//! `get_api_key()` calls read the new values lock-free.

use crate::auth::OAuthCredentials as AnthropicOAuthCredentials;
use crate::config::{ApiType, LlmConfig, ProviderConfig};
use crate::error::{LlmError, Result};
use crate::openai_auth::OAuthCredentials as OpenAiOAuthCredentials;

use anyhow::Context as _;
use arc_swap::ArcSwap;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::RwLock;

/// Manages LLM provider clients and tracks rate limit state.
pub struct LlmManager {
    config: ArcSwap<LlmConfig>,
    http_client: reqwest::Client,
    /// Models currently in rate limit cooldown, with the time they were limited.
    rate_limited: Arc<RwLock<HashMap<String, Instant>>>,
    /// Instance directory for reading/writing OAuth credentials.
    instance_dir: Option<PathBuf>,
    /// Cached Anthropic OAuth credentials (refreshed lazily).
    anthropic_oauth_credentials: RwLock<Option<AnthropicOAuthCredentials>>,
    /// Cached OpenAI OAuth credentials (refreshed lazily).
    openai_oauth_credentials: RwLock<Option<OpenAiOAuthCredentials>>,
}

impl LlmManager {
    /// Create a new LLM manager with the given configuration.
    pub async fn new(config: LlmConfig) -> Result<Self> {
        let http_client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(120))
            .build()
            .with_context(|| "failed to build HTTP client")?;

        Ok(Self {
            config: ArcSwap::from_pointee(config),
            http_client,
            rate_limited: Arc::new(RwLock::new(HashMap::new())),
            instance_dir: None,
            anthropic_oauth_credentials: RwLock::new(None),
            openai_oauth_credentials: RwLock::new(None),
        })
    }

    /// Set the instance directory and load any existing OAuth credentials.
    pub async fn set_instance_dir(&self, instance_dir: PathBuf) {
        if let Ok(Some(creds)) = crate::auth::load_credentials(&instance_dir) {
            tracing::info!("loaded Anthropic OAuth credentials from auth.json");
            *self.anthropic_oauth_credentials.write().await = Some(creds);
        }
        if let Ok(Some(creds)) = crate::openai_auth::load_credentials(&instance_dir) {
            tracing::info!("loaded OpenAI OAuth credentials from openai_chatgpt_oauth.json");
            *self.openai_oauth_credentials.write().await = Some(creds);
        }
        // Store instance_dir — we can't set it on &self since it's not behind RwLock,
        // but we only need it for save_credentials which we handle inline.
    }

    /// Initialize with an instance directory (for use at construction time).
    pub async fn with_instance_dir(config: LlmConfig, instance_dir: PathBuf) -> Result<Self> {
        let http_client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(120))
            .build()
            .with_context(|| "failed to build HTTP client")?;

        let anthropic_oauth_credentials = match crate::auth::load_credentials(&instance_dir) {
            Ok(Some(creds)) => {
                tracing::info!("loaded Anthropic OAuth credentials from auth.json");
                Some(creds)
            }
            Ok(None) => None,
            Err(error) => {
                tracing::warn!(%error, "failed to load Anthropic OAuth credentials");
                None
            }
        };

        let openai_oauth_credentials = match crate::openai_auth::load_credentials(&instance_dir) {
            Ok(Some(creds)) => {
                tracing::info!("loaded OpenAI OAuth credentials from openai_chatgpt_oauth.json");
                Some(creds)
            }
            Ok(None) => None,
            Err(error) => {
                tracing::warn!(%error, "failed to load OpenAI OAuth credentials");
                None
            }
        };

        Ok(Self {
            config: ArcSwap::from_pointee(config),
            http_client,
            rate_limited: Arc::new(RwLock::new(HashMap::new())),
            instance_dir: Some(instance_dir),
            anthropic_oauth_credentials: RwLock::new(anthropic_oauth_credentials),
            openai_oauth_credentials: RwLock::new(openai_oauth_credentials),
        })
    }

    /// Atomically swap in new provider credentials.
    pub fn reload_config(&self, config: LlmConfig) {
        self.config.store(Arc::new(config));
        tracing::info!("LLM provider keys reloaded");
    }

    pub fn get_provider(&self, provider_id: &str) -> Result<ProviderConfig> {
        let normalized_provider_id = provider_id.to_lowercase();
        let config = self.config.load();

        config
            .providers
            .get(&normalized_provider_id)
            .cloned()
            .ok_or_else(|| LlmError::UnknownProvider(provider_id.to_string()).into())
    }

    /// Get the appropriate API key for a provider, with OAuth override for Anthropic.
    ///
    /// If OAuth credentials are available and the provider is Anthropic,
    /// returns the OAuth access token (refreshing if needed). Otherwise
    /// falls back to the static API key from config.
    pub async fn get_anthropic_token(&self) -> Result<Option<String>> {
        let mut creds_guard = self.anthropic_oauth_credentials.write().await;
        let Some(creds) = creds_guard.as_ref() else {
            return Ok(None);
        };

        if !creds.is_expired() {
            return Ok(Some(creds.access_token.clone()));
        }

        // Need to refresh
        tracing::info!("Anthropic OAuth access token expired, refreshing...");
        match creds.refresh().await {
            Ok(new_creds) => {
                // Save to disk
                if let Some(ref instance_dir) = self.instance_dir
                    && let Err(error) = crate::auth::save_credentials(instance_dir, &new_creds)
                {
                    tracing::warn!(%error, "failed to persist refreshed Anthropic OAuth credentials");
                }
                let token = new_creds.access_token.clone();
                *creds_guard = Some(new_creds);
                tracing::info!("Anthropic OAuth token refreshed successfully");
                Ok(Some(token))
            }
            Err(error) => {
                tracing::error!(%error, "Anthropic OAuth token refresh failed");
                // Return the expired token anyway — the API will reject it
                // and the error message will be clearer than "no key"
                Ok(Some(creds.access_token.clone()))
            }
        }
    }

    /// Resolve the Anthropic provider config, preferring OAuth credentials.
    ///
    /// If a static provider exists in config, returns it with the API key
    /// overridden by the OAuth token when available. If no static provider
    /// exists but OAuth credentials are present, builds a provider from
    /// the OAuth token alone.
    pub async fn get_anthropic_provider(&self) -> Result<ProviderConfig> {
        let token = self.get_anthropic_token().await?;
        let static_provider = self.get_provider("anthropic").ok();

        match (static_provider, token) {
            (Some(mut provider), Some(token)) => {
                provider.api_key = token;
                Ok(provider)
            }
            (Some(provider), None) => Ok(provider),
            (None, Some(token)) => Ok(ProviderConfig {
                api_type: ApiType::Anthropic,
                base_url: "https://api.anthropic.com".to_string(),
                api_key: token,
                name: None,
            }),
            (None, None) => Err(LlmError::UnknownProvider("anthropic".to_string()).into()),
        }
    }

    /// Set OpenAI OAuth credentials in memory after successful auth.
    pub async fn set_openai_oauth_credentials(&self, creds: OpenAiOAuthCredentials) {
        *self.openai_oauth_credentials.write().await = Some(creds);
    }

    /// Get OpenAI OAuth access token if available, refreshing when needed.
    pub async fn get_openai_token(&self) -> Result<Option<String>> {
        let mut creds_guard = self.openai_oauth_credentials.write().await;
        let Some(creds) = creds_guard.as_ref() else {
            return Ok(None);
        };

        if !creds.is_expired() {
            return Ok(Some(creds.access_token.clone()));
        }

        tracing::info!("OpenAI OAuth access token expired, refreshing...");
        match creds.refresh().await {
            Ok(new_creds) => {
                if let Some(ref instance_dir) = self.instance_dir
                    && let Err(error) =
                        crate::openai_auth::save_credentials(instance_dir, &new_creds)
                {
                    tracing::warn!(%error, "failed to persist refreshed OpenAI OAuth credentials");
                }
                let token = new_creds.access_token.clone();
                *creds_guard = Some(new_creds);
                tracing::info!("OpenAI OAuth token refreshed successfully");
                Ok(Some(token))
            }
            Err(error) => {
                tracing::error!(%error, "OpenAI OAuth token refresh failed");
                Ok(Some(creds.access_token.clone()))
            }
        }
    }

    /// Resolve the OpenAI provider config from static API-key configuration.
    ///
    /// OpenAI ChatGPT OAuth is intentionally handled via a separate internal
    /// provider (`openai-chatgpt`) so a saved OAuth token cannot shadow a
    /// configured `openai` API key.
    pub async fn get_openai_provider(&self) -> Result<ProviderConfig> {
        self.get_provider("openai")
    }

    /// Resolve the OpenAI ChatGPT OAuth provider config.
    ///
    /// This internal provider uses OAuth access tokens from ChatGPT Plus/Pro.
    pub async fn get_openai_chatgpt_provider(&self) -> Result<ProviderConfig> {
        let token = self.get_openai_token().await?;

        match token {
            Some(token) => Ok(ProviderConfig {
                api_type: ApiType::OpenAiResponses,
                base_url: "https://chatgpt.com/backend-api/codex".to_string(),
                api_key: token,
                name: None,
            }),
            None => Err(LlmError::UnknownProvider("openai-chatgpt".to_string()).into()),
        }
    }

    /// Get OpenAI OAuth account id (for ChatGPT Plus/Pro account scoping headers).
    pub async fn get_openai_account_id(&self) -> Option<String> {
        self.openai_oauth_credentials
            .read()
            .await
            .as_ref()
            .and_then(|credentials| credentials.account_id.clone())
    }

    /// Get the appropriate API key for a provider.
    pub fn get_api_key(&self, provider_id: &str) -> Result<String> {
        let provider = self.get_provider(provider_id)?;

        if provider.api_key.is_empty() {
            return Err(LlmError::MissingProviderKey(provider_id.to_string()).into());
        }

        Ok(provider.api_key)
    }

    /// Get configured Ollama base URL, if provided.
    pub fn ollama_base_url(&self) -> Option<String> {
        self.config.load().ollama_base_url.clone()
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
        self.rate_limited
            .write()
            .await
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
        self.rate_limited
            .write()
            .await
            .retain(|_, limited_at| limited_at.elapsed().as_secs() < cooldown_secs);
    }
}
