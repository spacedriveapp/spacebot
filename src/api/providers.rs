use super::state::ApiState;
use crate::openai_auth::DeviceTokenPollResult;

use anyhow::Context as _;
use axum::Json;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use rig::agent::AgentBuilder;
use rig::completion::{CompletionModel as _, Prompt as _};
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use tokio::time::sleep;
use uuid::Uuid;

use std::collections::HashMap;
use std::sync::{Arc, LazyLock};
use std::time::Duration;

const OPENAI_DEVICE_OAUTH_SESSION_TTL_SECS: i64 = 30 * 60;
const OPENAI_DEVICE_OAUTH_DEFAULT_POLL_INTERVAL_SECS: u64 = 5;
const OPENAI_DEVICE_OAUTH_SLOWDOWN_SECS: u64 = 5;
const OPENAI_DEVICE_OAUTH_MAX_POLL_INTERVAL_SECS: u64 = 30;

static OPENAI_DEVICE_OAUTH_SESSIONS: LazyLock<RwLock<HashMap<String, DeviceOAuthSession>>> =
    LazyLock::new(|| RwLock::new(HashMap::new()));

#[derive(Clone, Debug)]
struct DeviceOAuthSession {
    expires_at: i64,
    status: DeviceOAuthSessionStatus,
}

#[derive(Clone, Debug)]
enum DeviceOAuthSessionStatus {
    Pending,
    Completed(String),
    Failed(String),
}

#[derive(Serialize, utoipa::ToSchema)]
pub(super) struct ProviderStatus {
    anthropic: bool,
    openai: bool,
    openai_chatgpt: bool,
    openrouter: bool,
    kilo: bool,
    requesty: bool,
    zhipu: bool,
    groq: bool,
    together: bool,
    fireworks: bool,
    deepseek: bool,
    xai: bool,
    mistral: bool,
    gemini: bool,
    ollama: bool,
    opencode_zen: bool,
    opencode_go: bool,
    nvidia: bool,
    minimax: bool,
    minimax_cn: bool,
    moonshot: bool,
    zai_coding_plan: bool,
    github_copilot: bool,
    azure: bool,
}

#[derive(Serialize, utoipa::ToSchema)]
pub(super) struct ProvidersResponse {
    providers: ProviderStatus,
    has_any: bool,
}

#[derive(Deserialize, utoipa::ToSchema)]
pub(super) struct ProviderUpdateRequest {
    provider: String,
    api_key: String,
    model: String,
    // Azure-specific fields (optional, required for Azure)
    #[serde(default)]
    base_url: Option<String>,
    #[serde(default)]
    api_version: Option<String>,
    #[serde(default)]
    deployment: Option<String>,
}

#[derive(Serialize, Deserialize, utoipa::ToSchema)]
pub struct ProviderUpdateResponse {
    pub success: bool,
    pub message: String,
}

#[derive(Deserialize, utoipa::ToSchema)]
pub(super) struct ProviderModelTestRequest {
    provider: String,
    api_key: String,
    model: String,
    // Azure-specific fields (optional, required for Azure)
    #[serde(default)]
    base_url: Option<String>,
    #[serde(default)]
    api_version: Option<String>,
    #[serde(default)]
    deployment: Option<String>,
}

#[derive(Serialize, Deserialize, utoipa::ToSchema)]
pub struct ProviderModelTestResponse {
    pub success: bool,
    pub message: String,
    pub provider: String,
    pub model: String,
    pub sample: Option<String>,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct ProviderDefaultModelsResponse {
    /// Default model per provider id, from the backend routing defaults.
    pub defaults: HashMap<String, String>,
    /// Default model applied by the ChatGPT device OAuth flow.
    pub chatgpt_oauth: String,
}

#[derive(Deserialize, utoipa::ToSchema)]
pub(super) struct OpenAiOAuthBrowserStartRequest {
    /// Model to apply after sign-in. Omitted or empty means the backend's
    /// default for the ChatGPT OAuth provider.
    #[serde(default)]
    model: Option<String>,
}

#[derive(Serialize, utoipa::ToSchema)]
pub(super) struct OpenAiOAuthBrowserStartResponse {
    success: bool,
    message: String,
    user_code: Option<String>,
    verification_url: Option<String>,
    state: Option<String>,
}

#[derive(Deserialize, utoipa::ToSchema, utoipa::IntoParams)]
pub(super) struct OpenAiOAuthBrowserStatusRequest {
    state: String,
}

#[derive(Serialize, utoipa::ToSchema)]
pub(super) struct OpenAiOAuthBrowserStatusResponse {
    found: bool,
    done: bool,
    success: bool,
    message: Option<String>,
}

fn provider_toml_key(provider: &str) -> Option<&'static str> {
    match provider {
        "anthropic" => Some("anthropic_key"),
        "openai" => Some("openai_key"),
        "openrouter" => Some("openrouter_key"),
        "kilo" => Some("kilo_key"),
        "requesty" => Some("requesty_key"),
        "zhipu" => Some("zhipu_key"),
        "groq" => Some("groq_key"),
        "together" => Some("together_key"),
        "fireworks" => Some("fireworks_key"),
        "deepseek" => Some("deepseek_key"),
        "xai" => Some("xai_key"),
        "mistral" => Some("mistral_key"),
        "gemini" => Some("gemini_key"),
        "ollama" => Some("ollama_base_url"),
        "opencode-zen" => Some("opencode_zen_key"),
        "opencode-go" => Some("opencode_go_key"),
        "nvidia" => Some("nvidia_key"),
        "minimax" => Some("minimax_key"),
        "minimax-cn" => Some("minimax_cn_key"),
        "moonshot" => Some("moonshot_key"),
        "zai-coding-plan" => Some("zai_coding_plan_key"),
        "github-copilot" => Some("github_copilot_key"),
        _ => None,
    }
}

fn model_matches_provider(provider: &str, model: &str) -> bool {
    crate::llm::routing::provider_from_model(model) == provider
}

/// Reload the in-memory defaults config from disk so that newly created agents
/// inherit the latest routing values rather than stale startup defaults.
async fn refresh_defaults_config(state: &Arc<ApiState>) {
    let config_path = state.config_path.read().await.clone();
    if config_path.as_os_str().is_empty() || !config_path.exists() {
        return;
    }
    match crate::config::Config::load_from_path(&config_path) {
        Ok(new_config) => {
            state.set_defaults_config(new_config.defaults).await;
            tracing::debug!("defaults_config refreshed from config.toml");
        }
        Err(error) => {
            tracing::warn!(%error, "failed to refresh defaults_config from config.toml");
        }
    }
}

fn normalize_openai_chatgpt_model(model: &str) -> Option<String> {
    let trimmed = model.trim();
    let (provider, model_name) = trimmed.split_once('/')?;
    if model_name.is_empty() {
        return None;
    }

    match provider {
        "openai" => Some(format!("openai-chatgpt/{model_name}")),
        "openai-chatgpt" => Some(trimmed.to_string()),
        _ => None,
    }
}

fn build_test_llm_config(provider: &str, credential: &str) -> crate::config::LlmConfig {
    let mut providers = HashMap::new();
    if let Some(provider_config) = crate::config::default_provider_config(provider, credential) {
        providers.insert(provider.to_string(), provider_config);
    }

    crate::config::LlmConfig {
        anthropic_key: (provider == "anthropic").then(|| credential.to_string()),
        openai_key: (provider == "openai").then(|| credential.to_string()),
        openrouter_key: (provider == "openrouter").then(|| credential.to_string()),
        kilo_key: (provider == "kilo").then(|| credential.to_string()),
        requesty_key: (provider == "requesty").then(|| credential.to_string()),
        zhipu_key: (provider == "zhipu").then(|| credential.to_string()),
        groq_key: (provider == "groq").then(|| credential.to_string()),
        together_key: (provider == "together").then(|| credential.to_string()),
        fireworks_key: (provider == "fireworks").then(|| credential.to_string()),
        deepseek_key: (provider == "deepseek").then(|| credential.to_string()),
        xai_key: (provider == "xai").then(|| credential.to_string()),
        mistral_key: (provider == "mistral").then(|| credential.to_string()),
        gemini_key: (provider == "gemini").then(|| credential.to_string()),
        ollama_key: None,
        ollama_base_url: (provider == "ollama").then(|| credential.to_string()),
        opencode_zen_key: (provider == "opencode-zen").then(|| credential.to_string()),
        opencode_go_key: (provider == "opencode-go").then(|| credential.to_string()),
        nvidia_key: (provider == "nvidia").then(|| credential.to_string()),
        minimax_key: (provider == "minimax").then(|| credential.to_string()),
        minimax_cn_key: (provider == "minimax-cn").then(|| credential.to_string()),
        moonshot_key: (provider == "moonshot").then(|| credential.to_string()),
        zai_coding_plan_key: (provider == "zai-coding-plan").then(|| credential.to_string()),
        github_copilot_key: (provider == "github-copilot").then(|| credential.to_string()),
        providers,
    }
}

fn apply_model_routing(doc: &mut toml_edit::DocumentMut, model: &str) {
    if doc.get("defaults").is_none() {
        doc["defaults"] = toml_edit::Item::Table(toml_edit::Table::new());
    }
    if let Some(defaults) = doc.get_mut("defaults").and_then(|item| item.as_table_mut()) {
        if defaults.get("routing").is_none() {
            defaults["routing"] = toml_edit::Item::Table(toml_edit::Table::new());
        }
        if let Some(routing_table) = defaults
            .get_mut("routing")
            .and_then(|item| item.as_table_mut())
        {
            routing_table["channel"] = toml_edit::value(model);
            routing_table["branch"] = toml_edit::value(model);
            routing_table["worker"] = toml_edit::value(model);
            routing_table["compactor"] = toml_edit::value(model);
            routing_table["cortex"] = toml_edit::value(model);
        }
    }

    if let Some(agents) = doc
        .get_mut("agents")
        .and_then(|agents_item| agents_item.as_array_of_tables_mut())
        && let Some(default_agent) = agents.iter_mut().find(|agent| {
            agent
                .get("default")
                .and_then(|value| value.as_bool())
                .unwrap_or(false)
        })
    {
        if default_agent.get("routing").is_none() {
            default_agent["routing"] = toml_edit::Item::Table(toml_edit::Table::new());
        }
        if let Some(routing_table) = default_agent
            .get_mut("routing")
            .and_then(|routing_item| routing_item.as_table_mut())
        {
            routing_table["channel"] = toml_edit::value(model);
            routing_table["branch"] = toml_edit::value(model);
            routing_table["worker"] = toml_edit::value(model);
            routing_table["compactor"] = toml_edit::value(model);
            routing_table["cortex"] = toml_edit::value(model);
        }
    }
}

impl DeviceOAuthSession {
    fn is_expired(&self, now: i64) -> bool {
        now >= self.expires_at
    }
}

impl DeviceOAuthSessionStatus {
    fn is_pending(&self) -> bool {
        matches!(self, DeviceOAuthSessionStatus::Pending)
    }
}

async fn prune_expired_device_oauth_sessions() {
    let cutoff = chrono::Utc::now().timestamp() - OPENAI_DEVICE_OAUTH_SESSION_TTL_SECS;
    let mut sessions = OPENAI_DEVICE_OAUTH_SESSIONS.write().await;
    sessions.retain(|_, session| session.expires_at >= cutoff);
}

async fn is_device_oauth_session_pending(state_key: &str) -> bool {
    let sessions = OPENAI_DEVICE_OAUTH_SESSIONS.read().await;
    sessions
        .get(state_key)
        .is_some_and(|session| session.status.is_pending())
}

async fn update_device_oauth_status(state_key: &str, status: DeviceOAuthSessionStatus) {
    if let Some(session) = OPENAI_DEVICE_OAUTH_SESSIONS
        .write()
        .await
        .get_mut(state_key)
    {
        session.status = status;
    }
}

/// Outcome of walking the ChatGPT OAuth model candidates to find one the
/// account actually serves.
#[derive(Debug, PartialEq, Eq)]
enum ModelVerification {
    /// The requested model responded.
    Requested(String),
    /// The requested model was rejected and this default candidate responded.
    Fallback(String),
    /// No candidate responded. Each entry is `model: error`.
    Unverified(Vec<String>),
}

/// Try the requested model first, then the provider's default candidates, and
/// report the first one that responds. Only a model that reaches this point
/// verified is safe to write into routing.
async fn resolve_verified_model<Verify, Fut>(requested: &str, verify: Verify) -> ModelVerification
where
    Verify: Fn(String) -> Fut,
    Fut: Future<Output = Result<(), String>>,
{
    let mut candidates = vec![requested.to_string()];
    for candidate in crate::llm::routing::default_model_candidates("openai-chatgpt") {
        if !candidates.contains(&candidate) {
            candidates.push(candidate);
        }
    }

    let mut failures = Vec::new();
    for candidate in candidates {
        match verify(candidate.clone()).await {
            Ok(()) => {
                return if candidate == requested {
                    ModelVerification::Requested(candidate)
                } else {
                    ModelVerification::Fallback(candidate)
                };
            }
            Err(error) => {
                tracing::warn!(model = %candidate, %error, "ChatGPT OAuth model verification failed");
                failures.push(format!("{candidate}: {error}"));
            }
        }
    }

    ModelVerification::Unverified(failures)
}

/// Run a one-shot completion against the ChatGPT OAuth provider to prove a
/// model id is actually served for this account.
async fn verify_openai_chatgpt_model(
    llm_manager: &Arc<crate::llm::LlmManager>,
    model: &str,
) -> Result<(), String> {
    let model = crate::llm::SpacebotModel::make(llm_manager, model);
    let agent = AgentBuilder::new(model)
        .preamble("You are running a provider connectivity check. Reply with exactly: OK")
        .build();
    agent
        .prompt("Connection test")
        .await
        .map(|_| ())
        .map_err(|error| error.to_string())
}

async fn finalize_openai_oauth(
    state: &Arc<ApiState>,
    credentials: &crate::openai_auth::OAuthCredentials,
    model: &str,
) -> anyhow::Result<String> {
    let instance_dir = (**state.instance_dir.load()).clone();
    crate::openai_auth::save_credentials(&instance_dir, credentials)
        .context("failed to save OpenAI OAuth credentials")?;

    let llm_manager = match state.llm_manager.read().await.as_ref() {
        Some(llm_manager) => {
            llm_manager
                .set_openai_oauth_credentials(credentials.clone())
                .await;
            llm_manager.clone()
        }
        None => {
            // No manager yet (first-run setup) — build one just for verification.
            let llm_manager =
                Arc::new(crate::llm::LlmManager::new(build_test_llm_config("", "")).await?);
            llm_manager
                .set_openai_oauth_credentials(credentials.clone())
                .await;
            llm_manager
        }
    };

    // Verify the requested model before writing it into routing; if the
    // provider rejects it, walk the default candidates until one responds.
    let verification = resolve_verified_model(model, |candidate| {
        let llm_manager = llm_manager.clone();
        async move { verify_openai_chatgpt_model(&llm_manager, &candidate).await }
    })
    .await;

    let (applied_model, message) = match verification {
        ModelVerification::Requested(applied) => {
            let message = format!(
                "OpenAI configured via device OAuth. Model '{applied}' verified and applied to defaults and default agent routing."
            );
            (applied, message)
        }
        ModelVerification::Fallback(applied) => {
            let message = format!(
                "OpenAI configured via device OAuth. Model '{model}' was rejected by the provider, so verified fallback '{applied}' was applied to defaults and default agent routing instead."
            );
            (applied, message)
        }
        ModelVerification::Unverified(failures) => {
            // The credentials stay saved so the sign-in doesn't have to be
            // repeated, but routing keeps the models it already had rather than
            // pointing every slot at an id this account can't serve.
            anyhow::bail!(
                "can't apply model routing: this ChatGPT account served none of the candidate models ({}). Existing routing is unchanged — set a working model under Settings → Model Routing and report this as a bug.",
                failures.join("; ")
            );
        }
    };

    let config_path = state.config_path.read().await.clone();
    let content = if config_path.exists() {
        tokio::fs::read_to_string(&config_path)
            .await
            .context("failed to read config.toml")?
    } else {
        String::new()
    };

    let mut doc: toml_edit::DocumentMut = content.parse().context("failed to parse config.toml")?;
    apply_model_routing(&mut doc, &applied_model);
    tokio::fs::write(&config_path, doc.to_string())
        .await
        .context("failed to write config.toml")?;

    // Refresh in-memory defaults so newly created agents inherit the updated routing.
    refresh_defaults_config(state).await;

    state
        .provider_setup_tx
        .try_send(crate::ProviderSetupEvent::ProvidersConfigured)
        .ok();

    Ok(message)
}

#[utoipa::path(
    get,
    path = "/providers",
    responses(
        (status = 200, body = ProvidersResponse),
        (status = 500, description = "Internal server error"),
    ),
    tag = "providers",
)]
pub(super) async fn get_providers(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ProvidersResponse>, StatusCode> {
    let config_path = state.config_path.read().await.clone();
    let instance_dir = (**state.instance_dir.load()).clone();
    let secrets_store = state.secrets_store.load();
    let openai_oauth_configured = crate::openai_auth::credentials_path(&instance_dir).exists();
    let anthropic_oauth_configured = crate::auth::credentials_path(&instance_dir).exists();
    let env_set = |name: &str| {
        std::env::var(name)
            .ok()
            .is_some_and(|value| !value.trim().is_empty())
    };

    let (
        anthropic,
        openai,
        openai_chatgpt,
        openrouter,
        kilo,
        requesty,
        zhipu,
        groq,
        together,
        fireworks,
        deepseek,
        xai,
        mistral,
        gemini,
        ollama,
        opencode_zen,
        opencode_go,
        nvidia,
        minimax,
        minimax_cn,
        moonshot,
        zai_coding_plan,
        github_copilot,
        azure,
    ) = if config_path.exists() {
        let content = tokio::fs::read_to_string(&config_path).await.map_err(|error| {
            tracing::error!(%error, path = %config_path.display(), "failed to read config.toml for providers");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;
        let doc: toml_edit::DocumentMut = content.parse().map_err(|error| {
            tracing::error!(%error, "failed to parse config.toml for providers");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

        let resolve_value = |value: &str| -> Option<String> {
            if let Some(alias) = value.strip_prefix("secret:") {
                let store = secrets_store.as_ref().as_ref()?;
                return match store.get(&crate::secrets::store::SecretScope::shared(), alias) {
                    Ok(secret) => Some(secret.expose().to_string()),
                    Err(error) => {
                        tracing::warn!(%error, alias, "failed to resolve secret reference");
                        None
                    }
                };
            }

            if let Some(var_name) = value.strip_prefix("env:") {
                return std::env::var(var_name)
                    .ok()
                    .filter(|resolved| !resolved.trim().is_empty());
            }

            if value.trim().is_empty() {
                None
            } else {
                Some(value.to_string())
            }
        };

        let has_value = |key: &str, env_var: &str| -> bool {
            if let Some(llm) = doc.get("llm")
                && let Some(val) = llm.get(key)
                && let Some(s) = val.as_str()
            {
                return resolve_value(s).is_some();
            }
            env_set(env_var)
        };

        (
            has_value("anthropic_key", "ANTHROPIC_API_KEY") || anthropic_oauth_configured,
            has_value("openai_key", "OPENAI_API_KEY"),
            openai_oauth_configured,
            has_value("openrouter_key", "OPENROUTER_API_KEY"),
            has_value("kilo_key", "KILO_API_KEY"),
            has_value("requesty_key", "REQUESTY_API_KEY"),
            has_value("zhipu_key", "ZHIPU_API_KEY"),
            has_value("groq_key", "GROQ_API_KEY"),
            has_value("together_key", "TOGETHER_API_KEY"),
            has_value("fireworks_key", "FIREWORKS_API_KEY"),
            has_value("deepseek_key", "DEEPSEEK_API_KEY"),
            has_value("xai_key", "XAI_API_KEY"),
            has_value("mistral_key", "MISTRAL_API_KEY"),
            has_value("gemini_key", "GEMINI_API_KEY"),
            has_value("ollama_base_url", "OLLAMA_BASE_URL")
                || has_value("ollama_key", "OLLAMA_API_KEY"),
            has_value("opencode_zen_key", "OPENCODE_ZEN_API_KEY"),
            has_value("opencode_go_key", "OPENCODE_GO_API_KEY"),
            has_value("nvidia_key", "NVIDIA_API_KEY"),
            has_value("minimax_key", "MINIMAX_API_KEY"),
            has_value("minimax_cn_key", "MINIMAX_CN_API_KEY"),
            has_value("moonshot_key", "MOONSHOT_API_KEY"),
            has_value("zai_coding_plan_key", "ZAI_CODING_PLAN_API_KEY"),
            has_value("github_copilot_key", "GITHUB_COPILOT_API_KEY"),
            doc.get("llm")
                .and_then(|llm| llm.get("provider"))
                .and_then(|provider| provider.get("azure"))
                .and_then(|azure| azure.get("base_url"))
                .and_then(|base_url| base_url.as_str())
                .is_some_and(|url| !url.trim().is_empty()),
        )
    } else {
        (
            env_set("ANTHROPIC_API_KEY") || anthropic_oauth_configured,
            env_set("OPENAI_API_KEY"),
            openai_oauth_configured,
            env_set("OPENROUTER_API_KEY"),
            env_set("KILO_API_KEY"),
            env_set("REQUESTY_API_KEY"),
            env_set("ZHIPU_API_KEY"),
            env_set("GROQ_API_KEY"),
            env_set("TOGETHER_API_KEY"),
            env_set("FIREWORKS_API_KEY"),
            env_set("DEEPSEEK_API_KEY"),
            env_set("XAI_API_KEY"),
            env_set("MISTRAL_API_KEY"),
            env_set("GEMINI_API_KEY"),
            env_set("OLLAMA_BASE_URL") || env_set("OLLAMA_API_KEY"),
            env_set("OPENCODE_ZEN_API_KEY"),
            env_set("OPENCODE_GO_API_KEY"),
            env_set("NVIDIA_API_KEY"),
            env_set("MINIMAX_API_KEY"),
            env_set("MINIMAX_CN_API_KEY"),
            env_set("MOONSHOT_API_KEY"),
            env_set("ZAI_CODING_PLAN_API_KEY"),
            env_set("GITHUB_COPILOT_API_KEY"),
            false,
        )
    };

    let providers = ProviderStatus {
        anthropic,
        openai,
        openai_chatgpt,
        openrouter,
        kilo,
        requesty,
        zhipu,
        groq,
        together,
        fireworks,
        deepseek,
        xai,
        mistral,
        gemini,
        ollama,
        opencode_zen,
        opencode_go,
        nvidia,
        minimax,
        minimax_cn,
        moonshot,
        zai_coding_plan,
        github_copilot,
        azure,
    };
    let has_any = providers.anthropic
        || providers.openai
        || providers.openai_chatgpt
        || providers.openrouter
        || providers.kilo
        || providers.requesty
        || providers.zhipu
        || providers.groq
        || providers.together
        || providers.fireworks
        || providers.deepseek
        || providers.xai
        || providers.mistral
        || providers.gemini
        || providers.ollama
        || providers.opencode_zen
        || providers.opencode_go
        || providers.nvidia
        || providers.minimax
        || providers.minimax_cn
        || providers.moonshot
        || providers.zai_coding_plan
        || providers.github_copilot
        || providers.azure;

    Ok(Json(ProvidersResponse { providers, has_any }))
}

#[utoipa::path(
    get,
    path = "/providers/default-models",
    responses(
        (status = 200, body = ProviderDefaultModelsResponse),
    ),
    tag = "providers",
)]
pub(super) async fn get_provider_default_models() -> Json<ProviderDefaultModelsResponse> {
    const PROVIDER_IDS: &[&str] = &[
        "anthropic",
        "openai",
        "openai-chatgpt",
        "openrouter",
        "kilo",
        "requesty",
        "zhipu",
        "groq",
        "together",
        "fireworks",
        "deepseek",
        "xai",
        "mistral",
        "gemini",
        "nvidia",
        "opencode-zen",
        "opencode-go",
        "minimax",
        "minimax-cn",
        "moonshot",
        "zai-coding-plan",
        "github-copilot",
        "ollama",
    ];

    let mut defaults: HashMap<String, String> = PROVIDER_IDS
        .iter()
        .map(|id| {
            (
                id.to_string(),
                crate::llm::routing::defaults_for_provider(id).channel,
            )
        })
        .collect();
    // Azure model ids name the user's own deployment, so this is a placeholder
    // rather than a routing default.
    defaults.insert("azure".to_string(), "azure/gpt-4o".to_string());

    let chatgpt_oauth = crate::llm::routing::defaults_for_provider("openai-chatgpt").channel;
    Json(ProviderDefaultModelsResponse {
        defaults,
        chatgpt_oauth,
    })
}

#[utoipa::path(
    post,
    path = "/providers/openai/browser-oauth/start",
    request_body = OpenAiOAuthBrowserStartRequest,
    responses(
        (status = 200, body = OpenAiOAuthBrowserStartResponse),
        (status = 400, description = "Invalid request"),
    ),
    tag = "providers",
)]
pub(super) async fn start_openai_browser_oauth(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<OpenAiOAuthBrowserStartRequest>,
) -> Result<Json<OpenAiOAuthBrowserStartResponse>, StatusCode> {
    let requested_model = request.model.unwrap_or_default();
    let chatgpt_model = if requested_model.trim().is_empty() {
        crate::llm::routing::defaults_for_provider("openai-chatgpt").channel
    } else {
        match normalize_openai_chatgpt_model(&requested_model) {
            Some(model) => model,
            None => {
                return Ok(Json(OpenAiOAuthBrowserStartResponse {
                    success: false,
                    message: format!(
                        "Model '{requested_model}' must use provider 'openai' or 'openai-chatgpt'."
                    ),
                    user_code: None,
                    verification_url: None,
                    state: None,
                }));
            }
        }
    };

    prune_expired_device_oauth_sessions().await;

    let device_code = match crate::openai_auth::request_device_code().await {
        Ok(device_code) => device_code,
        Err(error) => {
            return Ok(Json(OpenAiOAuthBrowserStartResponse {
                success: false,
                message: format!("Failed to start device authorization: {error}"),
                user_code: None,
                verification_url: None,
                state: None,
            }));
        }
    };

    if device_code.device_auth_id.trim().is_empty() || device_code.user_code.trim().is_empty() {
        return Ok(Json(OpenAiOAuthBrowserStartResponse {
            success: false,
            message: "Device authorization response was missing required fields.".to_string(),
            user_code: None,
            verification_url: None,
            state: None,
        }));
    }

    let now = chrono::Utc::now().timestamp();
    let expires_in = device_code
        .expires_in
        .unwrap_or(OPENAI_DEVICE_OAUTH_SESSION_TTL_SECS as u64);
    let expires_at = now + expires_in as i64;
    let poll_interval = device_code
        .interval
        .unwrap_or(OPENAI_DEVICE_OAUTH_DEFAULT_POLL_INTERVAL_SECS);
    let verification_url = crate::openai_auth::device_verification_url(&device_code);
    let state_key = Uuid::new_v4().to_string();

    OPENAI_DEVICE_OAUTH_SESSIONS.write().await.insert(
        state_key.clone(),
        DeviceOAuthSession {
            expires_at,
            status: DeviceOAuthSessionStatus::Pending,
        },
    );

    let state_clone = state.clone();
    let state_key_clone = state_key.clone();
    let device_auth_id = device_code.device_auth_id.clone();
    let user_code = device_code.user_code.clone();
    tokio::spawn(async move {
        run_device_oauth_background(
            state_clone,
            state_key_clone,
            device_auth_id,
            user_code,
            poll_interval,
            expires_at,
            chatgpt_model,
        )
        .await;
    });

    Ok(Json(OpenAiOAuthBrowserStartResponse {
        success: true,
        message: "Device authorization started".to_string(),
        user_code: Some(device_code.user_code),
        verification_url: Some(verification_url),
        state: Some(state_key),
    }))
}

async fn run_device_oauth_background(
    state: Arc<ApiState>,
    state_key: String,
    device_auth_id: String,
    user_code: String,
    mut poll_interval_secs: u64,
    expires_at: i64,
    model: String,
) {
    poll_interval_secs = poll_interval_secs.max(1);

    loop {
        if !is_device_oauth_session_pending(&state_key).await {
            return;
        }

        let now = chrono::Utc::now().timestamp();
        if now >= expires_at {
            update_device_oauth_status(
                &state_key,
                DeviceOAuthSessionStatus::Failed(
                    "Sign-in expired. Please start again.".to_string(),
                ),
            )
            .await;
            return;
        }

        sleep(Duration::from_secs(poll_interval_secs)).await;

        let poll_result = crate::openai_auth::poll_device_token(&device_auth_id, &user_code).await;
        let grant = match poll_result {
            Ok(DeviceTokenPollResult::Pending) => continue,
            Ok(DeviceTokenPollResult::SlowDown) => {
                poll_interval_secs = poll_interval_secs
                    .saturating_add(OPENAI_DEVICE_OAUTH_SLOWDOWN_SECS)
                    .min(OPENAI_DEVICE_OAUTH_MAX_POLL_INTERVAL_SECS);
                continue;
            }
            Ok(DeviceTokenPollResult::Approved(grant)) => grant,
            Err(error) => {
                let message = format!("Device authorization polling failed: {error}");
                tracing::warn!(%message, "OpenAI device OAuth polling failed");
                update_device_oauth_status(&state_key, DeviceOAuthSessionStatus::Failed(message))
                    .await;
                return;
            }
        };

        let credentials = match crate::openai_auth::exchange_device_code(
            &grant.authorization_code,
            &grant.code_verifier,
        )
        .await
        {
            Ok(credentials) => credentials,
            Err(error) => {
                let message = format!("Device code exchange failed: {error}");
                tracing::warn!(%message, "OpenAI device OAuth failed during token exchange");
                update_device_oauth_status(&state_key, DeviceOAuthSessionStatus::Failed(message))
                    .await;
                return;
            }
        };

        match finalize_openai_oauth(&state, &credentials, &model).await {
            Ok(message) => {
                update_device_oauth_status(
                    &state_key,
                    DeviceOAuthSessionStatus::Completed(message),
                )
                .await;
            }
            Err(error) => {
                let message =
                    format!("Device OAuth sign-in completed but finalization failed: {error}");
                tracing::warn!(%message, "OpenAI device OAuth finalization failed");
                update_device_oauth_status(&state_key, DeviceOAuthSessionStatus::Failed(message))
                    .await;
            }
        }

        return;
    }
}

#[utoipa::path(
    get,
    path = "/providers/openai/browser-oauth/status",
    params(
        ("state" = String, Query, description = "OAuth state parameter"),
    ),
    responses(
        (status = 200, body = OpenAiOAuthBrowserStatusResponse),
        (status = 400, description = "Invalid request"),
    ),
    tag = "providers",
)]
pub(super) async fn openai_browser_oauth_status(
    Query(request): Query<OpenAiOAuthBrowserStatusRequest>,
) -> Result<Json<OpenAiOAuthBrowserStatusResponse>, StatusCode> {
    prune_expired_device_oauth_sessions().await;
    if request.state.trim().is_empty() {
        return Ok(Json(OpenAiOAuthBrowserStatusResponse {
            found: false,
            done: false,
            success: false,
            message: Some("Missing OAuth state".to_string()),
        }));
    }

    let state_key = request.state.trim();
    let now = chrono::Utc::now().timestamp();
    let mut sessions = OPENAI_DEVICE_OAUTH_SESSIONS.write().await;
    let Some(session) = sessions.get_mut(state_key) else {
        return Ok(Json(OpenAiOAuthBrowserStatusResponse {
            found: false,
            done: false,
            success: false,
            message: None,
        }));
    };

    if session.status.is_pending() && session.is_expired(now) {
        session.status =
            DeviceOAuthSessionStatus::Failed("Sign-in expired. Please start again.".to_string());
    }

    let response = match &session.status {
        DeviceOAuthSessionStatus::Pending => OpenAiOAuthBrowserStatusResponse {
            found: true,
            done: false,
            success: false,
            message: None,
        },
        DeviceOAuthSessionStatus::Completed(message) => OpenAiOAuthBrowserStatusResponse {
            found: true,
            done: true,
            success: true,
            message: Some(message.clone()),
        },
        DeviceOAuthSessionStatus::Failed(message) => OpenAiOAuthBrowserStatusResponse {
            found: true,
            done: true,
            success: false,
            message: Some(message.clone()),
        },
    };
    Ok(Json(response))
}

#[utoipa::path(
    post,
    path = "/providers",
    request_body = ProviderUpdateRequest,
    responses(
        (status = 200, body = ProviderUpdateResponse),
        (status = 400, description = "Invalid request"),
    ),
    tag = "providers",
)]
pub(super) async fn update_provider(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<ProviderUpdateRequest>,
) -> Result<Json<ProviderUpdateResponse>, StatusCode> {
    let normalized_provider = request.provider.trim().to_lowercase();
    let normalized_model = request.model.trim().to_string();

    if normalized_provider == "azure" {
        let azure_request = ProviderUpdateRequest {
            provider: request.provider,
            api_key: request.api_key,
            model: request.model,
            base_url: request.base_url,
            api_version: request.api_version,
            deployment: request.deployment,
        };
        return update_azure_provider(state, azure_request, &normalized_model).await;
    }

    let Some(key_name) = provider_toml_key(&normalized_provider) else {
        return Ok(Json(ProviderUpdateResponse {
            success: false,
            message: format!("Unknown provider: {}", request.provider),
        }));
    };

    if request.api_key.trim().is_empty() {
        return Ok(Json(ProviderUpdateResponse {
            success: false,
            message: "API key cannot be empty".into(),
        }));
    }

    if request.model.trim().is_empty() {
        return Ok(Json(ProviderUpdateResponse {
            success: false,
            message: "Model cannot be empty".into(),
        }));
    }

    if !model_matches_provider(&normalized_provider, &normalized_model) {
        return Ok(Json(ProviderUpdateResponse {
            success: false,
            message: format!(
                "Model '{}' does not match provider '{}'.",
                request.model, request.provider
            ),
        }));
    }

    let config_path = state.config_path.read().await.clone();

    let content = if config_path.exists() {
        tokio::fs::read_to_string(&config_path).await.map_err(|error| {
            tracing::error!(%error, path = %config_path.display(), "failed to read config.toml for provider setup");
            StatusCode::INTERNAL_SERVER_ERROR
        })?
    } else {
        String::new()
    };

    let mut doc: toml_edit::DocumentMut = content.parse().map_err(|error| {
        tracing::error!(%error, "failed to parse config.toml for provider setup");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    if doc.get("llm").is_none() {
        doc["llm"] = toml_edit::Item::Table(toml_edit::Table::new());
    }

    doc["llm"][key_name] = toml_edit::value(request.api_key);
    apply_model_routing(&mut doc, &normalized_model);

    tokio::fs::write(&config_path, doc.to_string())
        .await
        .map_err(|error| {
            tracing::error!(%error, path = %config_path.display(), "failed to write config.toml for provider setup");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    // Refresh in-memory defaults so newly created agents inherit the updated routing.
    refresh_defaults_config(&state).await;

    state
        .provider_setup_tx
        .try_send(crate::ProviderSetupEvent::ProvidersConfigured)
        .ok();

    Ok(Json(ProviderUpdateResponse {
        success: true,
        message: format!(
            "Provider '{}' configured. Model '{}' verified and applied to defaults and the default agent routing.",
            request.provider, request.model
        ),
    }))
}

async fn update_azure_provider(
    state: Arc<ApiState>,
    request: ProviderUpdateRequest,
    normalized_model: &str,
) -> Result<Json<ProviderUpdateResponse>, StatusCode> {
    let base_url = request.base_url.as_ref().ok_or(StatusCode::BAD_REQUEST)?;
    let normalized_base_url = base_url.trim().trim_end_matches('/');

    if !normalized_base_url.ends_with(".openai.azure.com") {
        return Ok(Json(ProviderUpdateResponse {
            success: false,
            message: "Base URL must end with .openai.azure.com".to_string(),
        }));
    }

    let api_version = request
        .api_version
        .as_ref()
        .ok_or(StatusCode::BAD_REQUEST)?;
    let api_version_regex = regex::Regex::new(r"^\d{4}-\d{2}-\d{2}(-preview)?$").map_err(|e| {
        tracing::error!(error = %e, "failed to compile api_version regex");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    if !api_version_regex.is_match(api_version.trim()) {
        return Ok(Json(ProviderUpdateResponse {
            success: false,
            message: "API version must match format: YYYY-MM-DD or YYYY-MM-DD-preview".to_string(),
        }));
    }

    let deployment = request.deployment.as_ref().ok_or(StatusCode::BAD_REQUEST)?;
    let deployment_regex = regex::Regex::new(r"^[a-zA-Z0-9.-]+$").map_err(|e| {
        tracing::error!(error = %e, "failed to compile deployment regex");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    if !deployment_regex.is_match(deployment.trim()) {
        return Ok(Json(ProviderUpdateResponse {
            success: false,
            message: "Deployment name must contain only alphanumeric characters, hyphens, and dots"
                .to_string(),
        }));
    }

    if normalized_model.is_empty() {
        return Ok(Json(ProviderUpdateResponse {
            success: false,
            message: "Model cannot be empty".into(),
        }));
    }

    let normalized_deployment = request.deployment.as_ref().map(|s| s.trim()).unwrap_or("");
    let azure_model = format!("azure/{}", normalized_deployment);
    if !model_matches_provider("azure", &azure_model) {
        return Ok(Json(ProviderUpdateResponse {
            success: false,
            message: format!(
                "Deployment '{}' does not match provider 'azure'.",
                normalized_deployment
            ),
        }));
    }

    let config_path = state.config_path.read().await.clone();
    let content = if config_path.exists() {
        tokio::fs::read_to_string(&config_path).await.map_err(|error| {
            tracing::error!(%error, path = %config_path.display(), "failed to read config.toml for azure setup");
            StatusCode::INTERNAL_SERVER_ERROR
        })?
    } else {
        String::new()
    };

    let mut doc: toml_edit::DocumentMut = content.parse().map_err(|error| {
        tracing::error!(%error, "failed to parse config.toml for azure setup");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    // Determine the API key: use incoming if non-empty, otherwise preserve existing
    let api_key = if request.api_key.trim().is_empty() {
        // Read existing API key from config
        match doc
            .get("llm")
            .and_then(|llm| llm.get("provider"))
            .and_then(|provider| provider.get("azure"))
            .and_then(|azure| azure.get("api_key"))
            .and_then(|v| v.as_str())
            .map(String::from)
        {
            Some(key) => key,
            None => {
                return Ok(Json(ProviderUpdateResponse {
                    success: false,
                    message: "API key is required but no existing key found".to_string(),
                }));
            }
        }
    } else {
        request.api_key.trim().to_string()
    };

    if doc.get("llm").is_none() {
        doc["llm"] = toml_edit::Item::Table(toml_edit::Table::new());
    }
    if doc["llm"].get("provider").is_none() {
        doc["llm"]["provider"] = toml_edit::Item::Table(toml_edit::Table::new());
    }
    if doc["llm"]["provider"].get("azure").is_none() {
        doc["llm"]["provider"]["azure"] = toml_edit::Item::Table(toml_edit::Table::new());
    }

    // Just initialized above if it was missing, so the table is guaranteed to exist.
    let azure_table = doc["llm"]["provider"]["azure"]
        .as_table_mut()
        .expect("azure table must exist after initialization above");
    azure_table["api_type"] = toml_edit::value("azure");
    azure_table["base_url"] = toml_edit::value(base_url.trim());
    azure_table["api_key"] = toml_edit::value(api_key.trim());
    azure_table["api_version"] = toml_edit::value(api_version.trim());
    azure_table["deployment"] = toml_edit::value(deployment.trim());

    if doc.get("defaults").is_none() {
        doc["defaults"] = toml_edit::Item::Table(toml_edit::Table::new());
    }

    if let Some(defaults) = doc.get_mut("defaults").and_then(|item| item.as_table_mut()) {
        if defaults.get("routing").is_none() {
            defaults["routing"] = toml_edit::Item::Table(toml_edit::Table::new());
        }

        if let Some(routing_table) = defaults
            .get_mut("routing")
            .and_then(|item| item.as_table_mut())
        {
            if routing_table.get("channel").is_none() {
                routing_table["channel"] =
                    toml_edit::value(format!("azure/{}", normalized_deployment));
            }
            if routing_table.get("branch").is_none() {
                routing_table["branch"] =
                    toml_edit::value(format!("azure/{}", normalized_deployment));
            }
            if routing_table.get("worker").is_none() {
                routing_table["worker"] =
                    toml_edit::value(format!("azure/{}", normalized_deployment));
            }
            if routing_table.get("compactor").is_none() {
                routing_table["compactor"] =
                    toml_edit::value(format!("azure/{}", normalized_deployment));
            }
            if routing_table.get("cortex").is_none() {
                routing_table["cortex"] =
                    toml_edit::value(format!("azure/{}", normalized_deployment));
            }
        }
    }

    tokio::fs::write(&config_path, doc.to_string())
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "failed to write config.toml for azure provider");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    refresh_defaults_config(&state).await;

    state
        .provider_setup_tx
        .try_send(crate::ProviderSetupEvent::ProvidersConfigured)
        .ok();

    Ok(Json(ProviderUpdateResponse {
        success: true,
        message: format!(
            "Azure provider configured. Deployment '{}' with model '{}' verified and applied to defaults and the default agent routing.",
            deployment, normalized_model
        ),
    }))
}

#[derive(Serialize, Deserialize, utoipa::ToSchema)]
pub struct ProviderConfigResponse {
    pub success: bool,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deployment: Option<String>,
    // Note: api_key is intentionally excluded for security.
    // Credentials should never be returned to the client.
}

#[utoipa::path(
    get,
    path = "/providers/{provider}/config",
    responses(
        (status = 200, body = ProviderConfigResponse),
        (status = 404, description = "Provider not found"),
    ),
    tag = "providers",
    params(
        ("provider" = String, Path, description = "Provider ID"),
    ),
)]
pub(super) async fn get_provider_config(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path(provider): axum::extract::Path<String>,
) -> Result<Json<ProviderConfigResponse>, StatusCode> {
    let normalized_provider = provider.trim().to_lowercase();

    // Only Azure needs special config retrieval
    if normalized_provider != "azure" {
        return Ok(Json(ProviderConfigResponse {
            success: true,
            message: "No additional configuration needed for this provider".to_string(),
            base_url: None,
            api_version: None,
            deployment: None,
        }));
    }

    let config_path = state.config_path.read().await.clone();
    if !config_path.exists() {
        return Ok(Json(ProviderConfigResponse {
            success: false,
            message: "No config file found".to_string(),
            base_url: None,
            api_version: None,
            deployment: None,
        }));
    }

    let content = tokio::fs::read_to_string(&config_path).await.map_err(|error| {
        tracing::error!(%error, path = %config_path.display(), "failed to read config.toml for azure config");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let doc: toml_edit::DocumentMut = content.parse().map_err(|error| {
        tracing::error!(%error, "failed to parse config.toml for azure config");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    // Get Azure config from [llm.provider.azure]
    let azure_config = doc
        .get("llm")
        .and_then(|llm| llm.get("provider"))
        .and_then(|provider| provider.get("azure"));

    if let Some(azure_table) = azure_config.and_then(|item| item.as_table_like()) {
        let base_url = azure_table
            .get("base_url")
            .and_then(|v| v.as_str())
            .map(String::from);
        let api_version = azure_table
            .get("api_version")
            .and_then(|v| v.as_str())
            .map(String::from);
        let deployment = azure_table
            .get("deployment")
            .and_then(|v| v.as_str())
            .map(String::from);

        if base_url.is_some() || api_version.is_some() || deployment.is_some() {
            return Ok(Json(ProviderConfigResponse {
                success: true,
                message: "Azure configuration found".to_string(),
                base_url,
                api_version,
                deployment,
            }));
        }
    }

    Ok(Json(ProviderConfigResponse {
        success: false,
        message: "Azure configuration not found".to_string(),
        base_url: None,
        api_version: None,
        deployment: None,
    }))
}

#[utoipa::path(
    post,
    path = "/providers/test-model",
    request_body = ProviderModelTestRequest,
    responses(
        (status = 200, body = ProviderModelTestResponse),
        (status = 400, description = "Invalid request"),
    ),
    tag = "providers",
)]
pub(super) async fn test_provider_model(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<ProviderModelTestRequest>,
) -> Result<Json<ProviderModelTestResponse>, StatusCode> {
    let normalized_provider = request.provider.trim().to_lowercase();
    let normalized_model = request.model.trim().to_string();

    // Azure is handled specially and doesn't have a TOML key
    if normalized_provider == "azure" {
        // Azure validation happens later in the function
    } else if provider_toml_key(&normalized_provider).is_none() {
        return Ok(Json(ProviderModelTestResponse {
            success: false,
            message: format!("Unknown provider: {}", request.provider),
            provider: request.provider,
            model: request.model,
            sample: None,
        }));
    }

    // Determine the API key to use
    let api_key_to_use = if request.api_key.trim().is_empty() {
        if normalized_provider == "azure" {
            // For Azure, try to use the existing stored key from config
            let config_path = state.config_path.read().await.clone();
            if config_path.exists() {
                let content = tokio::fs::read_to_string(&config_path).await.ok();
                if let Some(doc) = content.and_then(|c| c.parse::<toml_edit::DocumentMut>().ok()) {
                    doc.get("llm")
                        .and_then(|llm| llm.get("provider"))
                        .and_then(|provider| provider.get("azure"))
                        .and_then(|azure| azure.get("api_key"))
                        .and_then(|v| v.as_str())
                        .map(String::from)
                } else {
                    None
                }
            } else {
                None
            }
        } else {
            None
        }
    } else {
        Some(request.api_key.trim().to_string())
    };

    // If no key found, return error
    let api_key = match api_key_to_use {
        Some(key) => key,
        None => {
            return Ok(Json(ProviderModelTestResponse {
                success: false,
                message: "API key is required but not provided".to_string(),
                provider: request.provider,
                model: request.model,
                sample: None,
            }));
        }
    };

    if normalized_model.is_empty() {
        return Ok(Json(ProviderModelTestResponse {
            success: false,
            message: "Model cannot be empty".to_string(),
            provider: request.provider,
            model: request.model,
            sample: None,
        }));
    }

    if !model_matches_provider(&normalized_provider, &normalized_model) {
        return Ok(Json(ProviderModelTestResponse {
            success: false,
            message: format!(
                "Model '{}' does not match provider '{}'.",
                normalized_model, request.provider
            ),
            provider: request.provider,
            model: request.model,
            sample: None,
        }));
    }

    if normalized_provider == "azure" {
        let base_url = request.base_url.as_ref().ok_or(StatusCode::BAD_REQUEST)?;
        let normalized_base_url = base_url.trim().trim_end_matches('/');

        if !normalized_base_url.ends_with(".openai.azure.com") {
            return Ok(Json(ProviderModelTestResponse {
                success: false,
                message: "Base URL must end with .openai.azure.com".to_string(),
                provider: request.provider,
                model: request.model,
                sample: None,
            }));
        }

        let api_version = request
            .api_version
            .as_ref()
            .ok_or(StatusCode::BAD_REQUEST)?;
        let api_version_regex =
            regex::Regex::new(r"^\d{4}-\d{2}-\d{2}(-preview)?$").map_err(|e| {
                tracing::error!(error = %e, "failed to compile api_version regex");
                StatusCode::INTERNAL_SERVER_ERROR
            })?;

        if !api_version_regex.is_match(api_version.trim()) {
            return Ok(Json(ProviderModelTestResponse {
                success: false,
                message: "API version must match format: YYYY-MM-DD or YYYY-MM-DD-preview"
                    .to_string(),
                provider: request.provider,
                model: request.model,
                sample: None,
            }));
        }

        let deployment = request.deployment.as_ref().ok_or(StatusCode::BAD_REQUEST)?;
        let deployment_regex = regex::Regex::new(r"^[a-zA-Z0-9.-]+$").map_err(|e| {
            tracing::error!(error = %e, "failed to compile deployment regex");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

        if !deployment_regex.is_match(deployment.trim()) {
            return Ok(Json(ProviderModelTestResponse {
                success: false,
                message:
                    "Deployment name must contain only alphanumeric characters, hyphens, and dots"
                        .to_string(),
                provider: request.provider,
                model: request.model,
                sample: None,
            }));
        }

        let llm_config = crate::config::LlmConfig {
            anthropic_key: None,
            openai_key: None,
            openrouter_key: None,
            kilo_key: None,
            requesty_key: None,
            zhipu_key: None,
            groq_key: None,
            together_key: None,
            fireworks_key: None,
            deepseek_key: None,
            xai_key: None,
            mistral_key: None,
            gemini_key: None,
            ollama_key: None,
            ollama_base_url: None,
            opencode_zen_key: None,
            opencode_go_key: None,
            nvidia_key: None,
            minimax_key: None,
            minimax_cn_key: None,
            moonshot_key: None,
            zai_coding_plan_key: None,
            github_copilot_key: None,
            providers: {
                let mut providers = HashMap::new();
                providers.insert(
                    "azure".to_string(),
                    crate::config::ProviderConfig {
                        api_type: crate::config::ApiType::Azure,
                        base_url: base_url.trim().to_string(),
                        api_key: api_key.trim().to_string(),
                        name: None,
                        use_bearer_auth: false,
                        extra_headers: Vec::new(),
                        api_version: Some(api_version.trim().to_string()),
                        deployment: Some(deployment.trim().to_string()),
                    },
                );
                providers
            },
        };

        let llm_manager = match crate::llm::LlmManager::new(llm_config).await {
            Ok(manager) => Arc::new(manager),
            Err(error) => {
                return Ok(Json(ProviderModelTestResponse {
                    success: false,
                    message: format!("Failed to initialize provider: {error}"),
                    provider: request.provider,
                    model: request.model,
                    sample: None,
                }));
            }
        };

        let model = crate::llm::SpacebotModel::make(&llm_manager, normalized_model);
        let agent = AgentBuilder::new(model)
            .preamble("You are running a provider connectivity check. Reply with exactly: OK")
            .build();

        return match agent.prompt("Connection test").await {
            Ok(sample) => Ok(Json(ProviderModelTestResponse {
                success: true,
                message: "Model responded successfully".to_string(),
                provider: request.provider,
                model: request.model,
                sample: Some(sample),
            })),
            Err(error) => Ok(Json(ProviderModelTestResponse {
                success: false,
                message: format!("Model test failed: {error}"),
                provider: request.provider,
                model: request.model,
                sample: None,
            })),
        };
    }

    let llm_config = build_test_llm_config(&normalized_provider, api_key.trim());
    let llm_manager = match crate::llm::LlmManager::new(llm_config).await {
        Ok(manager) => Arc::new(manager),
        Err(error) => {
            return Ok(Json(ProviderModelTestResponse {
                success: false,
                message: format!("Failed to initialize provider: {error}"),
                provider: request.provider,
                model: request.model,
                sample: None,
            }));
        }
    };

    let model = crate::llm::SpacebotModel::make(&llm_manager, normalized_model);
    let agent = AgentBuilder::new(model)
        .preamble("You are running a provider connectivity check. Reply with exactly: OK")
        .build();

    match agent.prompt("Connection test").await {
        Ok(sample) => Ok(Json(ProviderModelTestResponse {
            success: true,
            message: "Model responded successfully".to_string(),
            provider: request.provider,
            model: request.model,
            sample: Some(sample),
        })),
        Err(error) => Ok(Json(ProviderModelTestResponse {
            success: false,
            message: format!("Model test failed: {error}"),
            provider: request.provider,
            model: request.model,
            sample: None,
        })),
    }
}

#[utoipa::path(
    delete,
    path = "/providers/{provider}",
    params(
        ("provider" = String, Path, description = "Provider name to delete"),
    ),
    responses(
        (status = 200, body = ProviderUpdateResponse),
        (status = 400, description = "Invalid request"),
        (status = 404, description = "Provider not found"),
    ),
    tag = "providers",
)]
pub(super) async fn delete_provider(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path(provider): axum::extract::Path<String>,
) -> Result<Json<ProviderUpdateResponse>, StatusCode> {
    let provider = provider.trim().to_lowercase();
    // OpenAI ChatGPT OAuth credentials are stored as a separate JSON file,
    // not in the TOML config, so handle removal separately.
    if provider == "openai-chatgpt" {
        let instance_dir = (**state.instance_dir.load()).clone();
        let cred_path = crate::openai_auth::credentials_path(&instance_dir);
        if cred_path.exists() {
            tokio::fs::remove_file(&cred_path).await.map_err(|error| {
                tracing::error!(%error, path = %cred_path.display(), "failed to remove OpenAI OAuth credentials");
                StatusCode::INTERNAL_SERVER_ERROR
            })?;
        }
        if let Some(mgr) = state.llm_manager.read().await.as_ref() {
            mgr.clear_openai_oauth_credentials().await;
        }
        return Ok(Json(ProviderUpdateResponse {
            success: true,
            message: "ChatGPT Plus OAuth credentials removed".into(),
        }));
    }

    // Anthropic OAuth credentials are stored as a separate JSON file.
    // Remove it so that get_providers no longer reports Anthropic as configured.
    if provider == "anthropic" {
        let instance_dir = (**state.instance_dir.load()).clone();
        let cred_path = crate::auth::credentials_path(&instance_dir);
        if cred_path.exists() {
            tokio::fs::remove_file(&cred_path).await.map_err(|error| {
                tracing::error!(%error, path = %cred_path.display(), "failed to remove Anthropic OAuth credentials");
                StatusCode::INTERNAL_SERVER_ERROR
            })?;
        }
        // Fall through to also remove the TOML key if present.
    }

    // GitHub Copilot has a cached token file alongside the TOML key.
    // Remove both the TOML key and the cached token.
    if provider == "github-copilot" {
        let instance_dir = (**state.instance_dir.load()).clone();
        let token_path = crate::github_copilot_auth::credentials_path(&instance_dir);
        if token_path.exists() {
            tokio::fs::remove_file(&token_path).await.map_err(|error| {
                tracing::error!(%error, path = %token_path.display(), "failed to remove github copilot token");
                StatusCode::INTERNAL_SERVER_ERROR
            })?;
        }
        if let Some(manager) = state.llm_manager.read().await.as_ref() {
            manager.clear_copilot_token().await;
        }
    }

    if provider == "azure" {
        let config_path = state.config_path.read().await.clone();
        if !config_path.exists() {
            return Ok(Json(ProviderUpdateResponse {
                success: false,
                message: "No config file found".into(),
            }));
        }

        let content = tokio::fs::read_to_string(&config_path).await.map_err(|error| {
            tracing::error!(%error, path = %config_path.display(), "failed to read config.toml for azure removal");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

        let mut doc: toml_edit::DocumentMut = content.parse().map_err(|error| {
            tracing::error!(%error, "failed to parse config.toml for azure removal");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

        if let Some(llm) = doc.get_mut("llm")
            && let Some(llm_table) = llm.as_table_mut()
            && let Some(provider_table) = llm_table.get_mut("provider")
            && let Some(provider_tbl) = provider_table.as_table_mut()
        {
            provider_tbl.remove("azure");
            if provider_tbl.is_empty() {
                llm_table.remove("provider");
            }
        }

        tokio::fs::write(&config_path, doc.to_string())
            .await
            .map_err(|e| {
                tracing::error!(error = %e, "failed to write config after azure removal");
                StatusCode::INTERNAL_SERVER_ERROR
            })?;

        return Ok(Json(ProviderUpdateResponse {
            success: true,
            message: "Provider 'azure' removed".into(),
        }));
    }

    let Some(key_name) = provider_toml_key(&provider) else {
        return Ok(Json(ProviderUpdateResponse {
            success: false,
            message: format!("Unknown provider: {}", provider),
        }));
    };

    let config_path = state.config_path.read().await.clone();
    if !config_path.exists() {
        return Ok(Json(ProviderUpdateResponse {
            success: false,
            message: "No config file found".into(),
        }));
    }

    let content = tokio::fs::read_to_string(&config_path).await.map_err(|error| {
        tracing::error!(%error, path = %config_path.display(), "failed to read config.toml for provider removal");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let mut doc: toml_edit::DocumentMut = content.parse().map_err(|error| {
        tracing::error!(%error, "failed to parse config.toml for provider removal");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    if let Some(llm) = doc.get_mut("llm")
        && let Some(table) = llm.as_table_mut()
    {
        table.remove(key_name);
    }

    tokio::fs::write(&config_path, doc.to_string())
        .await
        .map_err(|error| {
            tracing::error!(%error, path = %config_path.display(), "failed to write config.toml for provider removal");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    Ok(Json(ProviderUpdateResponse {
        success: true,
        message: format!("Provider '{}' removed", provider),
    }))
}

#[cfg(test)]
mod tests {
    use super::{ModelVerification, build_test_llm_config, resolve_verified_model};

    use std::sync::Mutex;

    /// Records every candidate a verification run attempted, in order.
    fn record_attempts() -> Mutex<Vec<String>> {
        Mutex::new(Vec::new())
    }

    #[test]
    fn build_test_llm_config_registers_ollama_provider_from_base_url() {
        let config = build_test_llm_config("ollama", "http://remote-ollama.local:11434");
        let provider = config
            .providers
            .get("ollama")
            .expect("ollama provider should be registered");

        assert_eq!(provider.base_url, "http://remote-ollama.local:11434");
        assert_eq!(provider.api_key, "");
    }

    #[tokio::test]
    async fn resolve_verified_model_keeps_the_requested_model_when_it_responds() {
        let attempts = record_attempts();
        let verification = resolve_verified_model("openai-chatgpt/gpt-5.6-sol", |candidate| {
            attempts.lock().unwrap().push(candidate);
            async { Ok(()) }
        })
        .await;

        assert_eq!(
            verification,
            ModelVerification::Requested("openai-chatgpt/gpt-5.6-sol".into())
        );
        assert_eq!(attempts.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn resolve_verified_model_falls_back_when_the_requested_model_is_rejected() {
        let requested = "openai-chatgpt/gpt-5.3-codex";
        let verification = resolve_verified_model(requested, |candidate| async move {
            if candidate == requested {
                Err("model_not_found: the model does not exist".to_string())
            } else {
                Ok(())
            }
        })
        .await;

        let ModelVerification::Fallback(applied) = verification else {
            panic!("a default candidate should have verified");
        };
        assert_ne!(applied, requested);
        assert!(applied.starts_with("openai-chatgpt/"), "got {applied}");
    }

    #[tokio::test]
    async fn resolve_verified_model_reports_every_failure_when_nothing_responds() {
        let attempts = record_attempts();
        let verification = resolve_verified_model("openai-chatgpt/bogus", |candidate| {
            attempts.lock().unwrap().push(candidate.clone());
            async move { Err(format!("model_not_found: {candidate}")) }
        })
        .await;

        let ModelVerification::Unverified(failures) = verification else {
            panic!("no candidate responded, so nothing should be applied");
        };
        let attempts = attempts.lock().unwrap();
        assert!(
            attempts.len() > 1,
            "default candidates should be tried after the requested model"
        );
        assert_eq!(failures.len(), attempts.len());
        assert!(failures[0].starts_with("openai-chatgpt/bogus: "));
    }

    #[tokio::test]
    async fn resolve_verified_model_does_not_retry_a_requested_default() {
        let requested = crate::llm::routing::default_model_candidates("openai-chatgpt")
            .into_iter()
            .next()
            .expect("openai-chatgpt has default candidates");
        let attempts = record_attempts();
        resolve_verified_model(&requested, |candidate| {
            attempts.lock().unwrap().push(candidate);
            async { Err("model_not_found".to_string()) }
        })
        .await;

        let attempts = attempts.lock().unwrap();
        assert_eq!(
            attempts
                .iter()
                .filter(|candidate| **candidate == requested)
                .count(),
            1
        );
    }
}
