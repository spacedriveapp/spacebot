use super::state::ApiState;

use anyhow::Context as _;
use axum::Json;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Html;
use reqwest::Url;
use rig::agent::AgentBuilder;
use rig::completion::{CompletionModel as _, Prompt as _};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, LazyLock};
use tokio::sync::RwLock;

const OPENAI_BROWSER_OAUTH_SESSION_TTL_SECS: i64 = 15 * 60;
const OPENAI_BROWSER_OAUTH_REDIRECT_PATH: &str = "/providers/openai/oauth/browser/callback";

static OPENAI_BROWSER_OAUTH_SESSIONS: LazyLock<RwLock<HashMap<String, BrowserOAuthSession>>> =
    LazyLock::new(|| RwLock::new(HashMap::new()));

#[derive(Clone, Debug)]
struct BrowserOAuthSession {
    pkce_verifier: String,
    redirect_uri: String,
    model: String,
    created_at: i64,
    status: BrowserOAuthSessionStatus,
}

#[derive(Clone, Debug)]
enum BrowserOAuthSessionStatus {
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
    authorization_url: Option<String>,
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

#[derive(Deserialize)]
pub(super) struct OpenAiOAuthBrowserCallbackQuery {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
    error_description: Option<String>,
}

fn provider_toml_key(provider: &str) -> Option<&'static str> {
    match provider {
        "anthropic" => Some("anthropic_key"),
        "openai" => Some("openai_key"),
        "openrouter" => Some("openrouter_key"),
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
    use crate::config::{ApiType, ProviderConfig};

    let mut providers = HashMap::new();
    let provider_config = match provider {
        "anthropic" => Some(ProviderConfig {
            api_type: ApiType::Anthropic,
            base_url: "https://api.anthropic.com".to_string(),
            api_key: credential.to_string(),
            name: None,
        }),
        "openai" => Some(ProviderConfig {
            api_type: ApiType::OpenAiCompletions,
            base_url: "https://api.openai.com".to_string(),
            api_key: credential.to_string(),
            name: None,
        }),
        "openrouter" => Some(ProviderConfig {
            api_type: ApiType::OpenAiCompletions,
            base_url: "https://openrouter.ai/api".to_string(),
            api_key: credential.to_string(),
            name: None,
        }),
        "zhipu" => Some(ProviderConfig {
            api_type: ApiType::OpenAiCompletions,
            base_url: "https://api.z.ai/api/paas/v4".to_string(),
            api_key: credential.to_string(),
            name: None,
        }),
        "groq" => Some(ProviderConfig {
            api_type: ApiType::OpenAiCompletions,
            base_url: "https://api.groq.com/openai".to_string(),
            api_key: credential.to_string(),
            name: None,
        }),
        "together" => Some(ProviderConfig {
            api_type: ApiType::OpenAiCompletions,
            base_url: "https://api.together.xyz".to_string(),
            api_key: credential.to_string(),
            name: None,
        }),
        "fireworks" => Some(ProviderConfig {
            api_type: ApiType::OpenAiCompletions,
            base_url: "https://api.fireworks.ai/inference".to_string(),
            api_key: credential.to_string(),
            name: None,
        }),
        "deepseek" => Some(ProviderConfig {
            api_type: ApiType::OpenAiCompletions,
            base_url: "https://api.deepseek.com".to_string(),
            api_key: credential.to_string(),
            name: None,
        }),
        "xai" => Some(ProviderConfig {
            api_type: ApiType::OpenAiCompletions,
            base_url: "https://api.x.ai".to_string(),
            api_key: credential.to_string(),
            name: None,
        }),
        "mistral" => Some(ProviderConfig {
            api_type: ApiType::OpenAiCompletions,
            base_url: "https://api.mistral.ai".to_string(),
            api_key: credential.to_string(),
            name: None,
        }),
        "gemini" => Some(ProviderConfig {
            api_type: ApiType::Gemini,
            base_url: crate::config::GEMINI_PROVIDER_BASE_URL.to_string(),
            api_key: credential.to_string(),
            name: None,
        }),
        "opencode-zen" => Some(ProviderConfig {
            api_type: ApiType::OpenAiCompletions,
            base_url: "https://opencode.ai/zen".to_string(),
            api_key: credential.to_string(),
            name: None,
        }),
        "nvidia" => Some(ProviderConfig {
            api_type: ApiType::OpenAiCompletions,
            base_url: "https://integrate.api.nvidia.com".to_string(),
            api_key: credential.to_string(),
            name: None,
        }),
        "minimax" => Some(ProviderConfig {
            api_type: ApiType::Anthropic,
            base_url: "https://api.minimax.io/anthropic".to_string(),
            api_key: credential.to_string(),
            name: None,
        }),
        "minimax-cn" => Some(ProviderConfig {
            api_type: ApiType::Anthropic,
            base_url: "https://api.minimaxi.com/anthropic".to_string(),
            api_key: credential.to_string(),
            name: None,
        }),
        "moonshot" => Some(ProviderConfig {
            api_type: ApiType::OpenAiCompletions,
            base_url: "https://api.moonshot.ai".to_string(),
            api_key: credential.to_string(),
            name: None,
        }),
        "zai-coding-plan" => Some(ProviderConfig {
            api_type: ApiType::OpenAiCompletions,
            base_url: "https://api.z.ai/api/coding/paas/v4".to_string(),
            api_key: credential.to_string(),
            name: None,
        }),
        _ => None,
    };

    if let Some(provider_config) = provider_config {
        providers.insert(provider.to_string(), provider_config);
    }

    crate::config::LlmConfig {
        anthropic_key: (provider == "anthropic").then(|| credential.to_string()),
        openai_key: (provider == "openai").then(|| credential.to_string()),
        openrouter_key: (provider == "openrouter").then(|| credential.to_string()),
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

async fn prune_expired_browser_oauth_sessions() {
    let cutoff = chrono::Utc::now().timestamp() - OPENAI_BROWSER_OAUTH_SESSION_TTL_SECS;
    let mut sessions = OPENAI_BROWSER_OAUTH_SESSIONS.write().await;
    sessions.retain(|_, session| session.created_at >= cutoff);
}

fn resolve_browser_oauth_redirect_uri(headers: &HeaderMap) -> Option<String> {
    if let Some(origin) = header_value(headers, axum::http::header::ORIGIN.as_str())
        && let Ok(origin_url) = Url::parse(origin)
    {
        let origin = origin_url.origin().ascii_serialization();
        if origin != "null" {
            return Some(format!("{origin}{OPENAI_BROWSER_OAUTH_REDIRECT_PATH}"));
        }
    }

    if let (Some(proto), Some(host)) = (
        header_value(headers, "x-forwarded-proto"),
        header_value(headers, "x-forwarded-host"),
    ) {
        let proto = first_header_value(proto);
        let host = normalize_host(first_header_value(host));
        return Some(format!(
            "{proto}://{host}{OPENAI_BROWSER_OAUTH_REDIRECT_PATH}"
        ));
    }

    if let Some(host) = header_value(headers, "host") {
        let host = normalize_host(host);
        let scheme = if is_local_host(&host) {
            "http"
        } else {
            "https"
        };
        return Some(format!(
            "{scheme}://{host}{OPENAI_BROWSER_OAUTH_REDIRECT_PATH}"
        ));
    }

    None
}

fn header_value(headers: &HeaderMap, name: impl AsRef<str>) -> Option<&str> {
    headers
        .get(name.as_ref())
        .and_then(|value| value.to_str().ok())
}

fn first_header_value(value: &str) -> &str {
    value.split(',').next().map(str::trim).unwrap_or(value)
}

fn normalize_host(host: &str) -> String {
    let host = host.trim();
    let colon_count = host.matches(':').count();
    if colon_count > 1 && !host.starts_with('[') {
        format!("[{host}]")
    } else {
        host.to_string()
    }
}

fn is_local_host(host: &str) -> bool {
    let host = host
        .trim_start_matches('[')
        .trim_end_matches(']')
        .split(':')
        .next()
        .unwrap_or(host);
    matches!(host, "localhost" | "127.0.0.1" | "::1")
}

fn browser_oauth_success_html() -> String {
    r#"<!doctype html>
<html>
  <head>
    <meta charset="utf-8" />
    <title>Spacebot OpenAI Sign-in</title>
    <style>
      body { font-family: system-ui, -apple-system, sans-serif; margin: 0; background: #0f1115; color: #ecf0f1; display: grid; place-items: center; min-height: 100vh; }
      .card { max-width: 520px; padding: 28px; border: 1px solid #2b313a; border-radius: 12px; background: #161a21; text-align: center; }
      h1 { margin: 0 0 12px 0; font-size: 22px; }
      p { margin: 0; color: #b2bcc8; line-height: 1.45; }
    </style>
  </head>
  <body>
    <div class="card">
      <h1>Sign-in complete</h1>
      <p>You can close this window and return to Spacebot settings.</p>
    </div>
    <script>setTimeout(() => window.close(), 1800);</script>
  </body>
</html>"#
        .to_string()
}

fn browser_oauth_error_html(message: &str) -> String {
    let escaped = message
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;");
    format!(
        r#"<!doctype html>
<html>
  <head>
    <meta charset="utf-8" />
    <title>Spacebot OpenAI Sign-in</title>
    <style>
      body {{ font-family: system-ui, -apple-system, sans-serif; margin: 0; background: #0f1115; color: #ecf0f1; display: grid; place-items: center; min-height: 100vh; }}
      .card {{ max-width: 560px; padding: 28px; border: 1px solid #4a2d31; border-radius: 12px; background: #1f1416; }}
      h1 {{ margin: 0 0 12px 0; font-size: 22px; color: #ff7878; }}
      p {{ margin: 0; color: #e6b9b9; line-height: 1.45; white-space: pre-wrap; word-break: break-word; }}
    </style>
  </head>
  <body>
    <div class="card">
      <h1>Sign-in failed</h1>
      <p>{}</p>
    </div>
  </body>
</html>"#,
        escaped
    )
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
        || providers.nvidia
        || providers.minimax
        || providers.minimax_cn
        || providers.moonshot
        || providers.zai_coding_plan;

    Ok(Json(ProvidersResponse { providers, has_any }))
}

pub(super) async fn start_openai_browser_oauth(
    headers: HeaderMap,
    Json(request): Json<OpenAiOAuthBrowserStartRequest>,
) -> Result<Json<OpenAiOAuthBrowserStartResponse>, StatusCode> {
    if request.model.trim().is_empty() {
        return Ok(Json(OpenAiOAuthBrowserStartResponse {
            success: false,
            message: "Model cannot be empty".to_string(),
            authorization_url: None,
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
            authorization_url: None,
            state: None,
        }));
    };

    let Some(redirect_uri) = resolve_browser_oauth_redirect_uri(&headers) else {
        return Ok(Json(OpenAiOAuthBrowserStartResponse {
            success: false,
            message: "Unable to determine OAuth callback URL. Check your Host/Origin headers."
                .to_string(),
            authorization_url: None,
            state: None,
        }));
    };

    prune_expired_browser_oauth_sessions().await;
    let browser_authorization = crate::openai_auth::start_browser_authorization(&redirect_uri);
    let state_key = browser_authorization.state.clone();

    OPENAI_BROWSER_OAUTH_SESSIONS.write().await.insert(
        state_key.clone(),
        BrowserOAuthSession {
            pkce_verifier: browser_authorization.pkce_verifier,
            redirect_uri,
            model: chatgpt_model,
            created_at: chrono::Utc::now().timestamp(),
            status: BrowserOAuthSessionStatus::Pending,
        },
    );

    Ok(Json(OpenAiOAuthBrowserStartResponse {
        success: true,
        message: "OpenAI browser OAuth started".to_string(),
        authorization_url: Some(browser_authorization.authorization_url),
        state: Some(state_key),
    }))
}

pub(super) async fn openai_browser_oauth_status(
    Query(request): Query<OpenAiOAuthBrowserStatusRequest>,
) -> Result<Json<OpenAiOAuthBrowserStatusResponse>, StatusCode> {
    prune_expired_browser_oauth_sessions().await;
    if request.state.trim().is_empty() {
        return Ok(Json(OpenAiOAuthBrowserStatusResponse {
            found: false,
            done: false,
            success: false,
            message: Some("Missing OAuth state".to_string()),
        }));
    }

    let sessions = OPENAI_BROWSER_OAUTH_SESSIONS.read().await;
    let Some(session) = sessions.get(request.state.trim()) else {
        return Ok(Json(OpenAiOAuthBrowserStatusResponse {
            found: false,
            done: false,
            success: false,
            message: None,
        }));
    };

    let response = match &session.status {
        BrowserOAuthSessionStatus::Pending => OpenAiOAuthBrowserStatusResponse {
            found: true,
            done: false,
            success: false,
            message: None,
        },
        BrowserOAuthSessionStatus::Completed(message) => OpenAiOAuthBrowserStatusResponse {
            found: true,
            done: true,
            success: true,
            message: Some(message.clone()),
        },
        BrowserOAuthSessionStatus::Failed(message) => OpenAiOAuthBrowserStatusResponse {
            found: true,
            done: true,
            success: false,
            message: Some(message.clone()),
        },
    };
    Ok(Json(response))
}

pub(super) async fn openai_browser_oauth_callback(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<OpenAiOAuthBrowserCallbackQuery>,
) -> Html<String> {
    prune_expired_browser_oauth_sessions().await;

    let Some(state_key) = query
        .state
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
    else {
        return Html(browser_oauth_error_html("Missing OAuth state."));
    };

    if let Some(error_code) = query.error.as_deref() {
        let mut message = format!("OpenAI returned OAuth error: {}", error_code);
        if let Some(description) = query.error_description.as_deref() {
            message.push_str(&format!(" ({})", description));
        }
        if let Some(session) = OPENAI_BROWSER_OAUTH_SESSIONS
            .write()
            .await
            .get_mut(&state_key)
        {
            session.status = BrowserOAuthSessionStatus::Failed(message.clone());
        }
        return Html(browser_oauth_error_html(&message));
    }

    let Some(code) = query
        .code
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        let message = "OpenAI callback did not include an authorization code.";
        if let Some(session) = OPENAI_BROWSER_OAUTH_SESSIONS
            .write()
            .await
            .get_mut(&state_key)
        {
            session.status = BrowserOAuthSessionStatus::Failed(message.to_string());
        }
        return Html(browser_oauth_error_html(message));
    };

    let (pkce_verifier, redirect_uri, model) = {
        let sessions = OPENAI_BROWSER_OAUTH_SESSIONS.read().await;
        let Some(session) = sessions.get(&state_key) else {
            return Html(browser_oauth_error_html(
                "OAuth session expired or was not found. Start sign-in again.",
            ));
        };
        (
            session.pkce_verifier.clone(),
            session.redirect_uri.clone(),
            session.model.clone(),
        )
    };

    let credentials = match crate::openai_auth::exchange_browser_code(
        code,
        &redirect_uri,
        &pkce_verifier,
    )
    .await
    {
        Ok(credentials) => credentials,
        Err(error) => {
            let message = format!("Failed to exchange OpenAI authorization code: {error}");
            if let Some(session) = OPENAI_BROWSER_OAUTH_SESSIONS
                .write()
                .await
                .get_mut(&state_key)
            {
                session.status = BrowserOAuthSessionStatus::Failed(message.clone());
            }
            return Html(browser_oauth_error_html(&message));
        }
    };

    let persist_result = async {
        let instance_dir = (**state.instance_dir.load()).clone();
        crate::openai_auth::save_credentials(&instance_dir, &credentials)
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

        let mut doc: toml_edit::DocumentMut =
            content.parse().context("failed to parse config.toml")?;
        apply_model_routing(&mut doc, &model);
        tokio::fs::write(&config_path, doc.to_string())
            .await
            .context("failed to write config.toml")?;

        state
            .provider_setup_tx
            .try_send(crate::ProviderSetupEvent::ProvidersConfigured)
            .ok();

        anyhow::Ok(())
    }
    .await;

    match persist_result {
        Ok(()) => {
            if let Some(session) = OPENAI_BROWSER_OAUTH_SESSIONS
                .write()
                .await
                .get_mut(&state_key)
            {
                session.status = BrowserOAuthSessionStatus::Completed(format!(
                    "OpenAI configured via browser OAuth. Model '{}' applied to defaults and default agent routing.",
                    model
                ));
            }
            Html(browser_oauth_success_html())
        }
        Err(error) => {
            let message = format!("OAuth sign-in completed but finalization failed: {error}");
            if let Some(session) = OPENAI_BROWSER_OAUTH_SESSIONS
                .write()
                .await
                .get_mut(&state_key)
            {
                session.status = BrowserOAuthSessionStatus::Failed(message.clone());
            }
            Html(browser_oauth_error_html(&message))
        }
    }
}

pub(super) async fn update_provider(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<ProviderUpdateRequest>,
) -> Result<Json<ProviderUpdateResponse>, StatusCode> {
    let Some(key_name) = provider_toml_key(&request.provider) else {
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

    if !model_matches_provider(&request.provider, &request.model) {
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
    apply_model_routing(&mut doc, request.model.as_str());

    tokio::fs::write(&config_path, doc.to_string())
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

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
    if provider_toml_key(&request.provider).is_none() {
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

    if request.model.trim().is_empty() {
        return Ok(Json(ProviderModelTestResponse {
            success: false,
            message: "Model cannot be empty".to_string(),
            provider: request.provider,
            model: request.model,
            sample: None,
        }));
    }

    if !model_matches_provider(&request.provider, &request.model) {
        return Ok(Json(ProviderModelTestResponse {
            success: false,
            message: format!(
                "Model '{}' does not match provider '{}'.",
                request.model, request.provider
            ),
            provider: request.provider,
            model: request.model,
            sample: None,
        }));
    }

    let llm_config = build_test_llm_config(&request.provider, request.api_key.trim());
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

    let model = crate::llm::SpacebotModel::make(&llm_manager, request.model.clone());
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
