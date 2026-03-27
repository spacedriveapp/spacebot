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
use crate::github_copilot_auth::CopilotToken;
use crate::github_copilot_oauth::OAuthCredentials as CopilotOAuthCredentials;
use crate::openai_auth::OAuthCredentials as OpenAiOAuthCredentials;

use anyhow::Context as _;
use arc_swap::ArcSwap;
use std::collections::HashMap;
use std::path::PathBuf;

/// Editor version header for GitHub Copilot API requests.
/// Matches VSCode 1.96.2 which Copilot expects for IDE auth.
const COPILOT_EDITOR_VERSION: &str = "vscode/1.96.2";

/// Editor plugin version header for GitHub Copilot API requests.
/// Matches Copilot Chat extension version 0.26.7.
const COPILOT_EDITOR_PLUGIN_VERSION: &str = "copilot-chat/0.26.7";
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
    /// Cached GitHub Copilot API token (exchanged from PAT or OAuth token, refreshed lazily).
    copilot_token: RwLock<Option<CopilotToken>>,
    /// Cached GitHub Copilot OAuth credentials (from device code flow).
    copilot_oauth_credentials: RwLock<Option<CopilotOAuthCredentials>>,
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
            copilot_token: RwLock::new(None),
            copilot_oauth_credentials: RwLock::new(None),
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
        match crate::github_copilot_auth::load_cached_token(&instance_dir) {
            Ok(Some(token)) => {
                tracing::info!("loaded GitHub Copilot token from github_copilot_token.json");
                *self.copilot_token.write().await = Some(token);
            }
            Ok(None) => {
                tracing::debug!("no cached GitHub Copilot token found");
            }
            Err(error) => {
                tracing::warn!(%error, "failed to load GitHub Copilot token");
            }
        }
        match crate::github_copilot_oauth::load_credentials(&instance_dir) {
            Ok(Some(creds)) => {
                tracing::info!(
                    "loaded GitHub Copilot OAuth credentials from github_copilot_oauth.json"
                );
                *self.copilot_oauth_credentials.write().await = Some(creds);
            }
            Ok(None) => {
                tracing::debug!("no GitHub Copilot OAuth credentials found");
            }
            Err(error) => {
                tracing::warn!(%error, "failed to load GitHub Copilot OAuth credentials");
            }
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

        let copilot_token = match crate::github_copilot_auth::load_cached_token(&instance_dir) {
            Ok(Some(token)) => {
                tracing::info!("loaded GitHub Copilot token from github_copilot_token.json");
                Some(token)
            }
            Ok(None) => None,
            Err(error) => {
                tracing::warn!(%error, "failed to load GitHub Copilot token");
                None
            }
        };

        let copilot_oauth_credentials =
            match crate::github_copilot_oauth::load_credentials(&instance_dir) {
                Ok(Some(creds)) => {
                    tracing::info!(
                        "loaded GitHub Copilot OAuth credentials from github_copilot_oauth.json"
                    );
                    Some(creds)
                }
                Ok(None) => None,
                Err(error) => {
                    tracing::warn!(%error, "failed to load GitHub Copilot OAuth credentials");
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
            copilot_token: RwLock::new(copilot_token),
            copilot_oauth_credentials: RwLock::new(copilot_oauth_credentials),
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
                use_bearer_auth: false,
                extra_headers: vec![],
                api_version: None,
                deployment: None,
            }),
            (None, None) => Err(LlmError::UnknownProvider("anthropic".to_string()).into()),
        }
    }

    /// Set OpenAI OAuth credentials in memory after successful auth.
    pub async fn set_openai_oauth_credentials(&self, creds: OpenAiOAuthCredentials) {
        *self.openai_oauth_credentials.write().await = Some(creds);
    }

    /// Clear OpenAI OAuth credentials from memory.
    pub async fn clear_openai_oauth_credentials(&self) {
        *self.openai_oauth_credentials.write().await = None;
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
                use_bearer_auth: false,
                extra_headers: vec![],
                api_version: None,
                deployment: None,
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

    /// Set GitHub Copilot OAuth credentials in memory after successful device flow.
    pub async fn set_copilot_oauth_credentials(&self, creds: CopilotOAuthCredentials) {
        *self.copilot_oauth_credentials.write().await = Some(creds);
    }

    /// Clear GitHub Copilot OAuth credentials from memory.
    pub async fn clear_copilot_oauth_credentials(&self) {
        *self.copilot_oauth_credentials.write().await = None;
    }

    /// Check if GitHub Copilot OAuth credentials are available.
    pub async fn has_copilot_oauth_credentials(&self) -> bool {
        self.copilot_oauth_credentials
            .read()
            .await
            .as_ref()
            .is_some_and(|creds| !creds.access_token.is_empty())
    }

    /// Get the GitHub OAuth token from Copilot OAuth credentials (device flow).
    async fn get_copilot_oauth_token(&self) -> Option<String> {
        let creds_guard = self.copilot_oauth_credentials.read().await;
        creds_guard
            .as_ref()
            .filter(|creds| !creds.access_token.is_empty())
            .map(|creds| creds.access_token.clone())
    }

    /// Get a valid GitHub Copilot API token, exchanging/refreshing as needed.
    ///
    /// Resolution order:
    /// 1. OAuth credentials from device code flow (github_copilot_oauth.json)
    /// 2. Static PAT from config (github_copilot_key / GITHUB_COPILOT_API_KEY)
    ///
    /// Both paths use the same Copilot token exchange to get a short-lived API token.
    pub async fn get_copilot_token(&self) -> Result<Option<String>> {
        // Try OAuth credentials first
        let github_token = if let Some(oauth_token) = self.get_copilot_oauth_token().await {
            oauth_token
        } else {
            // Fall back to static PAT from config
            match self.get_provider("github-copilot") {
                Ok(provider) if !provider.api_key.is_empty() => provider.api_key,
                _ => return Ok(None),
            }
        };

        let pat_hash = crate::github_copilot_auth::hash_pat(&github_token);

        // Check cached token — must be unexpired AND for the same PAT/OAuth token
        {
            let token_guard = self.copilot_token.read().await;
            if let Some(ref cached) = *token_guard
                && !cached.is_expired()
                && cached.pat_hash == pat_hash
            {
                return Ok(Some(cached.token.clone()));
            }
        } // read lock dropped here before network call

        // Need to exchange
        tracing::info!("exchanging GitHub token for Copilot API token...");
        match crate::github_copilot_auth::exchange_github_token(
            &self.http_client,
            &github_token,
            pat_hash.clone(),
        )
        .await
        {
            Ok(new_token) => {
                let api_token = new_token.token.clone();
                // Save to disk
                if let Some(ref instance_dir) = self.instance_dir
                    && let Err(error) =
                        crate::github_copilot_auth::save_cached_token(instance_dir, &new_token)
                {
                    tracing::warn!(%error, "failed to persist GitHub Copilot token");
                }
                // Update cache with write lock held only for the assignment
                *self.copilot_token.write().await = Some(new_token);
                tracing::info!("GitHub Copilot token exchanged successfully");
                Ok(Some(api_token))
            }
            Err(error) => {
                tracing::error!(%error, "GitHub Copilot token exchange failed");
                // Only fall back to cached token if it matches the current PAT hash
                let token_guard = self.copilot_token.read().await;
                if let Some(ref cached) = *token_guard
                    && cached.pat_hash == pat_hash
                {
                    return Ok(Some(cached.token.clone()));
                }
                Err(error.into())
            }
        }
    }

    /// Resolve the GitHub Copilot provider config with a fresh API token.
    ///
    /// Exchanges the stored GitHub PAT for a Copilot API token, derives the
    /// base URL from the token's `proxy-ep` field, and returns a complete
    /// `ProviderConfig` ready for OpenAI-compatible API calls.
    pub async fn get_github_copilot_provider(&self) -> Result<ProviderConfig> {
        let token = self
            .get_copilot_token()
            .await?
            .ok_or_else(|| LlmError::UnknownProvider("github-copilot".to_string()))?;

        let base_url = crate::github_copilot_auth::derive_base_url_from_token(&token)
            .unwrap_or_else(|| {
                crate::github_copilot_auth::DEFAULT_COPILOT_API_BASE_URL.to_string()
            });

        Ok(ProviderConfig {
            api_type: ApiType::OpenAiChatCompletions,
            base_url,
            api_key: token,
            name: Some("GitHub Copilot".to_string()),
            use_bearer_auth: true,
            extra_headers: vec![
                (
                    "user-agent".to_string(),
                    format!("spacebot/{}", env!("CARGO_PKG_VERSION")),
                ),
                (
                    "editor-version".to_string(),
                    COPILOT_EDITOR_VERSION.to_string(),
                ),
                (
                    "editor-plugin-version".to_string(),
                    COPILOT_EDITOR_PLUGIN_VERSION.to_string(),
                ),
            ],
            api_version: None,
            deployment: None,
        })
    }

    /// Clear cached GitHub Copilot token from memory only.
    ///
    /// Note: Does not delete the on-disk cache file. Use
    /// `github_copilot_auth::credentials_path()` and delete the file separately
    /// if persistent removal is needed (e.g., in `delete_provider`).
    pub async fn clear_copilot_token(&self) {
        *self.copilot_token.write().await = None;
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
