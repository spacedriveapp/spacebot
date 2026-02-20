//! LLM manager for provider credentials and HTTP client.
//!
//! The manager is intentionally simple — it holds API keys, an HTTP client,
//! and shared rate limit state. Routing decisions (which model for which
//! process) live on the agent's RoutingConfig, not here.
//!
//! API keys are hot-reloadable via ArcSwap. The file watcher calls
//! `reload_config()` when config.toml changes, and all subsequent
//! `get_api_key()` calls read the new values lock-free.

use crate::auth::OAuthCredentials;
use crate::config::{LlmConfig, ProviderConfig};
use crate::error::{LlmError, Result};

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
    /// Cached OAuth credentials (refreshed lazily).
    oauth_credentials: RwLock<Option<OAuthCredentials>>,
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
            oauth_credentials: RwLock::new(None),
        })
    }

    /// Set the instance directory and load any existing OAuth credentials.
    pub async fn set_instance_dir(&self, instance_dir: PathBuf) {
        if let Ok(Some(creds)) = crate::auth::load_credentials(&instance_dir) {
            tracing::info!("loaded OAuth credentials from auth.json");
            *self.oauth_credentials.write().await = Some(creds);
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

        let oauth_credentials = match crate::auth::load_credentials(&instance_dir) {
            Ok(Some(creds)) => {
                tracing::info!("loaded OAuth credentials from auth.json");
                Some(creds)
            }
            Ok(None) => None,
            Err(error) => {
                tracing::warn!(%error, "failed to load OAuth credentials");
                None
            }
        };

        Ok(Self {
            config: ArcSwap::from_pointee(config),
            http_client,
            rate_limited: Arc::new(RwLock::new(HashMap::new())),
            instance_dir: Some(instance_dir),
            oauth_credentials: RwLock::new(oauth_credentials),
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
        let mut creds_guard = self.oauth_credentials.write().await;
        let Some(creds) = creds_guard.as_ref() else {
            return Ok(None);
        };

        if !creds.is_expired() {
            return Ok(Some(creds.access_token.clone()));
        }

        // Need to refresh
        tracing::info!("OAuth access token expired, refreshing...");
        match creds.refresh().await {
            Ok(new_creds) => {
                // Save to disk
                if let Some(ref instance_dir) = self.instance_dir {
                    if let Err(error) = crate::auth::save_credentials(instance_dir, &new_creds) {
                        tracing::warn!(%error, "failed to persist refreshed OAuth credentials");
                    }
                }
                let token = new_creds.access_token.clone();
                *creds_guard = Some(new_creds);
                tracing::info!("OAuth token refreshed successfully");
                Ok(Some(token))
            }
            Err(error) => {
                tracing::error!(%error, "OAuth token refresh failed");
                // Return the expired token anyway — the API will reject it
                // and the error message will be clearer than "no key"
                Ok(Some(creds.access_token.clone()))
            }
        }
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
