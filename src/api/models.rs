use super::state::ApiState;

use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Serialize, Clone)]
pub(super) struct ModelInfo {
    /// Full routing string (e.g. "openrouter/anthropic/claude-sonnet-4-20250514")
    id: String,
    /// Human-readable name
    name: String,
    /// Provider ID for routing ("anthropic", "openrouter", "openai", etc.)
    provider: String,
    /// Context window size in tokens, if known
    context_window: Option<u64>,
    /// Whether this model supports tool/function calling
    tool_call: bool,
    /// Whether this model has reasoning/thinking capability
    reasoning: bool,
}

#[derive(Serialize)]
pub(super) struct ModelsResponse {
    models: Vec<ModelInfo>,
}

#[derive(Deserialize)]
struct ModelsDevProvider {
    #[allow(dead_code)]
    id: Option<String>,
    #[allow(dead_code)]
    name: Option<String>,
    #[serde(default)]
    models: HashMap<String, ModelsDevModel>,
}

#[derive(Deserialize)]
struct ModelsDevModel {
    #[allow(dead_code)]
    id: Option<String>,
    name: String,
    #[serde(default)]
    tool_call: bool,
    #[serde(default)]
    reasoning: bool,
    limit: Option<ModelsDevLimit>,
    modalities: Option<ModelsDevModalities>,
    status: Option<String>,
}

#[derive(Deserialize)]
struct ModelsDevLimit {
    context: u64,
}

#[derive(Deserialize)]
struct ModelsDevModalities {
    #[allow(dead_code)]
    input: Option<Vec<String>>,
    output: Option<Vec<String>>,
}

/// Cached model catalog fetched from models.dev.
static MODELS_CACHE: std::sync::LazyLock<
    tokio::sync::RwLock<(Vec<ModelInfo>, std::time::Instant)>,
> = std::sync::LazyLock::new(|| {
    tokio::sync::RwLock::new((Vec::new(), std::time::Instant::now()))
});

const MODELS_CACHE_TTL: std::time::Duration = std::time::Duration::from_secs(3600);

/// Maps models.dev provider IDs to spacebot's internal provider IDs for
/// providers with direct integrations.
fn direct_provider_mapping(models_dev_id: &str) -> Option<&'static str> {
    match models_dev_id {
        "anthropic" => Some("anthropic"),
        "openai" => Some("openai"),
        "deepseek" => Some("deepseek"),
        "xai" => Some("xai"),
        "mistral" => Some("mistral"),
        "groq" => Some("groq"),
        "togetherai" => Some("together"),
        "fireworks-ai" => Some("fireworks"),
        "zhipuai" => Some("zhipu"),
        _ => None,
    }
}

/// Models from providers not in models.dev (private/custom endpoints).
fn extra_models() -> Vec<ModelInfo> {
    vec![
        ModelInfo {
            id: "opencode-zen/kimi-k2.5".into(),
            name: "Kimi K2.5".into(),
            provider: "opencode-zen".into(),
            context_window: None,
            tool_call: true,
            reasoning: true,
        },
        ModelInfo {
            id: "opencode-zen/kimi-k2".into(),
            name: "Kimi K2".into(),
            provider: "opencode-zen".into(),
            context_window: None,
            tool_call: true,
            reasoning: false,
        },
        ModelInfo {
            id: "opencode-zen/kimi-k2-thinking".into(),
            name: "Kimi K2 Thinking".into(),
            provider: "opencode-zen".into(),
            context_window: None,
            tool_call: true,
            reasoning: true,
        },
        ModelInfo {
            id: "opencode-zen/glm-5".into(),
            name: "GLM 5".into(),
            provider: "opencode-zen".into(),
            context_window: None,
            tool_call: true,
            reasoning: false,
        },
        ModelInfo {
            id: "opencode-zen/minimax-m2.5".into(),
            name: "MiniMax M2.5".into(),
            provider: "opencode-zen".into(),
            context_window: None,
            tool_call: true,
            reasoning: false,
        },
        ModelInfo {
            id: "opencode-zen/qwen3-coder".into(),
            name: "Qwen3 Coder 480B".into(),
            provider: "opencode-zen".into(),
            context_window: None,
            tool_call: true,
            reasoning: false,
        },
        ModelInfo {
            id: "opencode-zen/big-pickle".into(),
            name: "Big Pickle".into(),
            provider: "opencode-zen".into(),
            context_window: None,
            tool_call: true,
            reasoning: false,
        },
    ]
}

/// Fetch the full model catalog from models.dev and transform into ModelInfo entries.
async fn fetch_models_dev() -> anyhow::Result<Vec<ModelInfo>> {
    let client = reqwest::Client::new();
    let response = client
        .get("https://models.dev/api.json")
        .timeout(std::time::Duration::from_secs(15))
        .send()
        .await?
        .error_for_status()?;

    let catalog: HashMap<String, ModelsDevProvider> = response.json().await?;
    let mut models = Vec::new();

    for (provider_id, provider) in &catalog {
        for (model_id, model) in &provider.models {
            if model.status.as_deref() == Some("deprecated") {
                continue;
            }

            let has_text_output = model
                .modalities
                .as_ref()
                .and_then(|m| m.output.as_ref())
                .is_some_and(|outputs| outputs.iter().any(|o| o == "text"));
            if !has_text_output {
                continue;
            }

            let (routing_id, routing_provider) =
                if let Some(spacebot_provider) = direct_provider_mapping(provider_id) {
                    (
                        format!("{spacebot_provider}/{model_id}"),
                        spacebot_provider.to_string(),
                    )
                } else if provider_id == "openrouter" {
                    (format!("openrouter/{model_id}"), "openrouter".into())
                } else {
                    (
                        format!("openrouter/{provider_id}/{model_id}"),
                        "openrouter".into(),
                    )
                };

            let context_window = model.limit.as_ref().map(|l| l.context);

            models.push(ModelInfo {
                id: routing_id,
                name: model.name.clone(),
                provider: routing_provider,
                context_window,
                tool_call: model.tool_call,
                reasoning: model.reasoning,
            });
        }
    }

    models.sort_by(|a, b| a.provider.cmp(&b.provider).then(a.name.cmp(&b.name)));

    Ok(models)
}

/// Ensure the cache is populated (fetches on first call, then uses TTL).
async fn ensure_models_cache() -> Vec<ModelInfo> {
    {
        let cache = MODELS_CACHE.read().await;
        if !cache.0.is_empty() && cache.1.elapsed() < MODELS_CACHE_TTL {
            return cache.0.clone();
        }
    }

    match fetch_models_dev().await {
        Ok(models) => {
            let mut cache = MODELS_CACHE.write().await;
            *cache = (models.clone(), std::time::Instant::now());
            models
        }
        Err(error) => {
            tracing::warn!(%error, "failed to fetch models from models.dev, using stale cache");
            let cache = MODELS_CACHE.read().await;
            cache.0.clone()
        }
    }
}

/// Helper: which providers have keys configured.
pub(super) async fn configured_providers(config_path: &std::path::Path) -> Vec<&'static str> {
    let mut providers = Vec::new();

    let content = match tokio::fs::read_to_string(config_path).await {
        Ok(c) => c,
        Err(_) => return providers,
    };
    let doc: toml_edit::DocumentMut = match content.parse() {
        Ok(d) => d,
        Err(_) => return providers,
    };

    let has_key = |key: &str, env_var: &str| -> bool {
        if let Some(llm) = doc.get("llm") {
            if let Some(val) = llm.get(key) {
                if let Some(s) = val.as_str() {
                    if let Some(var_name) = s.strip_prefix("env:") {
                        return std::env::var(var_name).is_ok();
                    }
                    return !s.is_empty();
                }
            }
        }
        std::env::var(env_var).is_ok()
    };

    if has_key("anthropic_key", "ANTHROPIC_API_KEY") { providers.push("anthropic"); }
    if has_key("openai_key", "OPENAI_API_KEY") { providers.push("openai"); }
    if has_key("openrouter_key", "OPENROUTER_API_KEY") { providers.push("openrouter"); }
    if has_key("zhipu_key", "ZHIPU_API_KEY") { providers.push("zhipu"); }
    if has_key("groq_key", "GROQ_API_KEY") { providers.push("groq"); }
    if has_key("together_key", "TOGETHER_API_KEY") { providers.push("together"); }
    if has_key("fireworks_key", "FIREWORKS_API_KEY") { providers.push("fireworks"); }
    if has_key("deepseek_key", "DEEPSEEK_API_KEY") { providers.push("deepseek"); }
    if has_key("xai_key", "XAI_API_KEY") { providers.push("xai"); }
    if has_key("mistral_key", "MISTRAL_API_KEY") { providers.push("mistral"); }
    if has_key("opencode_zen_key", "OPENCODE_ZEN_API_KEY") { providers.push("opencode-zen"); }

    providers
}

pub(super) async fn get_models(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ModelsResponse>, StatusCode> {
    let config_path = state.config_path.read().await.clone();
    let configured = configured_providers(&config_path).await;

    let catalog = ensure_models_cache().await;

    let mut models: Vec<ModelInfo> = catalog
        .into_iter()
        .filter(|m| configured.contains(&m.provider.as_str()))
        .collect();

    for model in extra_models() {
        if configured.contains(&model.provider.as_str()) {
            models.push(model);
        }
    }

    Ok(Json(ModelsResponse { models }))
}

pub(super) async fn refresh_models(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ModelsResponse>, StatusCode> {
    {
        let mut cache = MODELS_CACHE.write().await;
        *cache = (Vec::new(), std::time::Instant::now() - MODELS_CACHE_TTL);
    }

    get_models(State(state)).await
}
