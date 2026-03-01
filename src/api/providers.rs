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

#[derive(Serialize)]
pub(super) struct ProviderStatus {
    anthropic: bool,
    openai: bool,
    openai_chatgpt: bool,
    openrouter: bool,
    kilo: bool,
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
}

#[derive(Serialize)]
pub(super) struct ProvidersResponse {
    providers: ProviderStatus,
    has_any: bool,
}

#[derive(Deserialize)]
pub(super) struct ProviderUpdateRequest {
    provider: String,
    api_key: String,
    model: String,
}

#[derive(Serialize)]
pub(super) struct ProviderUpdateResponse {
    success: bool,
    message: String,
}

#[derive(Deserialize)]
pub(super) struct ProviderModelTestRequest {
    provider: String,
    api_key: String,
    model: String,
}

#[derive(Serialize)]
pub(super) struct ProviderModelTestResponse {
    success: bool,
    message: String,
    provider: String,
    model: String,
    sample: Option<String>,
}

#[derive(Deserialize)]
pub(super) struct OpenAiOAuthBrowserStartRequest {
    model: String,
}

#[derive(Serialize)]
pub(super) struct OpenAiOAuthBrowserStartResponse {
    success: bool,
    message: String,
    user_code: Option<String>,
    verification_url: Option<String>,
    state: Option<String>,
}

#[derive(Deserialize)]
pub(super) struct OpenAiOAuthBrowserStatusRequest {
    state: String,
}

#[derive(Serialize)]
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

async fn finalize_openai_oauth(
    state: &Arc<ApiState>,
    credentials: &crate::openai_auth::OAuthCredentials,
    model: &str,
) -> anyhow::Result<()> {
    let instance_dir = (**state.instance_dir.load()).clone();
    crate::openai_auth::save_credentials(&instance_dir, credentials)
        .context("failed to save OpenAI OAuth credentials")?;

    if let Some(llm_manager) = state.llm_manager.read().await.as_ref() {
        llm_manager
            .set_openai_oauth_credentials(credentials.clone())
            .await;
    }

    let config_path = state.config_path.read().await.clone();
    let content = if config_path.exists() {
        tokio::fs::read_to_string(&config_path)
            .await
            .context("failed to read config.toml")?
    } else {
        String::new()
    };

    let mut doc: toml_edit::DocumentMut = content.parse().context("failed to parse config.toml")?;
    apply_model_routing(&mut doc, model);
    tokio::fs::write(&config_path, doc.to_string())
        .await
        .context("failed to write config.toml")?;

    // Refresh in-memory defaults so newly created agents inherit the updated routing.
    refresh_defaults_config(state).await;

    state
        .provider_setup_tx
        .try_send(crate::ProviderSetupEvent::ProvidersConfigured)
        .ok();

    Ok(())
}

pub(super) async fn get_providers(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ProvidersResponse>, StatusCode> {
    let config_path = state.config_path.read().await.clone();
    let instance_dir = (**state.instance_dir.load()).clone();
    let openai_oauth_configured = crate::openai_auth::credentials_path(&instance_dir).exists();

    let (
        anthropic,
        openai,
        openai_chatgpt,
        openrouter,
        kilo,
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
    ) = if config_path.exists() {
        let content = tokio::fs::read_to_string(&config_path)
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        let doc: toml_edit::DocumentMut = content
            .parse()
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

        let has_value = |key: &str, env_var: &str| -> bool {
            if let Some(llm) = doc.get("llm")
                && let Some(val) = llm.get(key)
                && let Some(s) = val.as_str()
            {
                if let Some(var_name) = s.strip_prefix("env:") {
                    return std::env::var(var_name).is_ok();
                }
                return !s.is_empty();
            }
            std::env::var(env_var).is_ok()
        };

        (
            has_value("anthropic_key", "ANTHROPIC_API_KEY"),
            has_value("openai_key", "OPENAI_API_KEY"),
            openai_oauth_configured,
            has_value("openrouter_key", "OPENROUTER_API_KEY"),
            has_value("kilo_key", "KILO_API_KEY"),
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
        )
    } else {
        (
            std::env::var("ANTHROPIC_API_KEY").is_ok(),
            std::env::var("OPENAI_API_KEY").is_ok(),
            openai_oauth_configured,
            std::env::var("OPENROUTER_API_KEY").is_ok(),
            std::env::var("KILO_API_KEY").is_ok(),
            std::env::var("ZHIPU_API_KEY").is_ok(),
            std::env::var("GROQ_API_KEY").is_ok(),
            std::env::var("TOGETHER_API_KEY").is_ok(),
            std::env::var("FIREWORKS_API_KEY").is_ok(),
            std::env::var("DEEPSEEK_API_KEY").is_ok(),
            std::env::var("XAI_API_KEY").is_ok(),
            std::env::var("MISTRAL_API_KEY").is_ok(),
            std::env::var("GEMINI_API_KEY").is_ok(),
            std::env::var("OLLAMA_BASE_URL").is_ok() || std::env::var("OLLAMA_API_KEY").is_ok(),
            std::env::var("OPENCODE_ZEN_API_KEY").is_ok(),
            std::env::var("OPENCODE_GO_API_KEY").is_ok(),
            std::env::var("NVIDIA_API_KEY").is_ok(),
            std::env::var("MINIMAX_API_KEY").is_ok(),
            std::env::var("MINIMAX_CN_API_KEY").is_ok(),
            std::env::var("MOONSHOT_API_KEY").is_ok(),
            std::env::var("ZAI_CODING_PLAN_API_KEY").is_ok(),
        )
    };

    let providers = ProviderStatus {
        anthropic,
        openai,
        openai_chatgpt,
        openrouter,
        kilo,
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
    };
    let has_any = providers.anthropic
        || providers.openai
        || providers.openai_chatgpt
        || providers.openrouter
        || providers.kilo
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
        || providers.zai_coding_plan;

    Ok(Json(ProvidersResponse { providers, has_any }))
}

pub(super) async fn start_openai_browser_oauth(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<OpenAiOAuthBrowserStartRequest>,
) -> Result<Json<OpenAiOAuthBrowserStartResponse>, StatusCode> {
    if request.model.trim().is_empty() {
        return Ok(Json(OpenAiOAuthBrowserStartResponse {
            success: false,
            message: "Model cannot be empty".to_string(),
            user_code: None,
            verification_url: None,
            state: None,
        }));
    }
    let Some(chatgpt_model) = normalize_openai_chatgpt_model(&request.model) else {
        return Ok(Json(OpenAiOAuthBrowserStartResponse {
            success: false,
            message: format!(
                "Model '{}' must use provider 'openai' or 'openai-chatgpt'.",
                request.model
            ),
            user_code: None,
            verification_url: None,
            state: None,
        }));
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
            Ok(()) => {
                update_device_oauth_status(
                    &state_key,
                    DeviceOAuthSessionStatus::Completed(format!(
                        "OpenAI configured via device OAuth. Model '{}' applied to defaults and default agent routing.",
                        model
                    )),
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

pub(super) async fn update_provider(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<ProviderUpdateRequest>,
) -> Result<Json<ProviderUpdateResponse>, StatusCode> {
    let normalized_provider = request.provider.trim().to_lowercase();
    let normalized_model = request.model.trim();
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

    if !model_matches_provider(&normalized_provider, normalized_model) {
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
        tokio::fs::read_to_string(&config_path)
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    } else {
        String::new()
    };

    let mut doc: toml_edit::DocumentMut = content
        .parse()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    if doc.get("llm").is_none() {
        doc["llm"] = toml_edit::Item::Table(toml_edit::Table::new());
    }

    doc["llm"][key_name] = toml_edit::value(request.api_key);
    apply_model_routing(&mut doc, normalized_model);

    tokio::fs::write(&config_path, doc.to_string())
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

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

pub(super) async fn test_provider_model(
    Json(request): Json<ProviderModelTestRequest>,
) -> Result<Json<ProviderModelTestResponse>, StatusCode> {
    let normalized_provider = request.provider.trim().to_lowercase();
    let normalized_model = request.model.trim().to_string();
    if provider_toml_key(&normalized_provider).is_none() {
        return Ok(Json(ProviderModelTestResponse {
            success: false,
            message: format!("Unknown provider: {}", request.provider),
            provider: request.provider,
            model: request.model,
            sample: None,
        }));
    }

    if request.api_key.trim().is_empty() {
        return Ok(Json(ProviderModelTestResponse {
            success: false,
            message: "API key cannot be empty".to_string(),
            provider: request.provider,
            model: request.model,
            sample: None,
        }));
    }

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

    let llm_config = build_test_llm_config(&normalized_provider, request.api_key.trim());
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
            tokio::fs::remove_file(&cred_path)
                .await
                .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        }
        if let Some(mgr) = state.llm_manager.read().await.as_ref() {
            mgr.clear_openai_oauth_credentials().await;
        }
        return Ok(Json(ProviderUpdateResponse {
            success: true,
            message: "ChatGPT Plus OAuth credentials removed".into(),
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

    let content = tokio::fs::read_to_string(&config_path)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let mut doc: toml_edit::DocumentMut = content
        .parse()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    if let Some(llm) = doc.get_mut("llm")
        && let Some(table) = llm.as_table_mut()
    {
        table.remove(key_name);
    }

    tokio::fs::write(&config_path, doc.to_string())
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(Json(ProviderUpdateResponse {
        success: true,
        message: format!("Provider '{}' removed", provider),
    }))
}

#[cfg(test)]
mod tests {
    use super::build_test_llm_config;

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
}
