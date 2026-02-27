use super::state::ApiState;

use axum::Json;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Serialize, Clone)]
pub(super) struct ModelInfo {
    /// Full routing string (e.g. "openrouter/anthropic/claude-sonnet-4")
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
    /// Whether this model accepts audio input.
    input_audio: bool,
}

#[derive(Serialize)]
pub(super) struct ModelsResponse {
    models: Vec<ModelInfo>,
}

#[derive(Deserialize)]
pub(super) struct ModelsQuery {
    provider: Option<String>,
    capability: Option<String>,
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
    input: Option<Vec<String>>,
    output: Option<Vec<String>>,
}

/// Cached model catalog fetched from models.dev.
static MODELS_CACHE: std::sync::LazyLock<
    tokio::sync::RwLock<(Vec<ModelInfo>, std::time::Instant)>,
> = std::sync::LazyLock::new(|| tokio::sync::RwLock::new((Vec::new(), std::time::Instant::now())));

const MODELS_CACHE_TTL: std::time::Duration = std::time::Duration::from_secs(3600);

/// Models known to work with Spacebot's current voice transcription path
/// (OpenAI-compatible `/v1/chat/completions` with `input_audio`).
const KNOWN_VOICE_TRANSCRIPTION_MODELS: &[&str] = &[
    // Native Gemini API
    "gemini/gemini-2.0-flash",
    "gemini/gemini-2.5-flash",
    "gemini/gemini-2.5-flash-lite",
    "gemini/gemini-2.5-pro",
    "gemini/gemini-3-flash-preview",
    "gemini/gemini-3-pro-preview",
    "gemini/gemini-3.1-pro-preview",
    // Via OpenRouter
    "openrouter/google/gemini-2.0-flash-001",
    "openrouter/google/gemini-2.5-flash",
    "openrouter/google/gemini-2.5-flash-lite",
    "openrouter/google/gemini-2.5-pro",
    "openrouter/google/gemini-3-flash-preview",
    "openrouter/google/gemini-3-pro-preview",
    "openrouter/google/gemini-3.1-pro-preview",
];

/// Maps models.dev provider IDs to spacebot's internal provider IDs for
/// providers with direct integrations.
fn direct_provider_mapping(models_dev_id: &str) -> Option<&'static str> {
    match models_dev_id {
        "anthropic" => Some("anthropic"),
        "openai" => Some("openai"),
        "kilo" => Some("kilo"),
        "deepseek" => Some("deepseek"),
        "xai" => Some("xai"),
        "mistral" => Some("mistral"),
        "gemini" | "google" => Some("gemini"),
        "groq" => Some("groq"),
        "togetherai" => Some("together"),
        "fireworks-ai" => Some("fireworks"),
        "zhipuai" => Some("zhipu"),
        "opencode" => Some("opencode-zen"),
        "opencode-go" => Some("opencode-go"),
        "zai-coding-plan" => Some("zai-coding-plan"),
        "minimax" => Some("minimax"),
        "moonshotai" => Some("moonshot"),
        _ => None,
    }
}

fn is_known_voice_transcription_model(model_id: &str) -> bool {
    KNOWN_VOICE_TRANSCRIPTION_MODELS.contains(&model_id)
}

fn as_openai_chatgpt_model(model: &ModelInfo) -> Option<ModelInfo> {
    if model.provider != "openai" {
        return None;
    }

    let model_name = model.id.strip_prefix("openai/")?;
    Some(ModelInfo {
        id: format!("openai-chatgpt/{model_name}"),
        name: model.name.clone(),
        provider: "openai-chatgpt".into(),
        context_window: model.context_window,
        tool_call: model.tool_call,
        reasoning: model.reasoning,
        input_audio: model.input_audio,
    })
}

/// Models from providers not in models.dev (private/custom endpoints).
fn extra_models() -> Vec<ModelInfo> {
    vec![
        // MiniMax CN - China-specific endpoint, not on models.dev
        ModelInfo {
            id: "minimax-cn/MiniMax-M2.5".into(),
            name: "MiniMax M2.5".into(),
            provider: "minimax-cn".into(),
            context_window: Some(200000),
            tool_call: true,
            reasoning: true,
            input_audio: false,
        },
        // Moonshot AI (Kimi) - moonshot-v1-8k not on models.dev
        ModelInfo {
            id: "moonshot/moonshot-v1-8k".into(),
            name: "Moonshot V1 8K".into(),
            provider: "moonshot".into(),
            context_window: Some(8000),
            tool_call: false,
            reasoning: false,
            input_audio: false,
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
            let input_audio = model
                .modalities
                .as_ref()
                .and_then(|m| m.input.as_ref())
                .is_some_and(|inputs| {
                    inputs
                        .iter()
                        .any(|input| input.to_lowercase().contains("audio"))
                });

            models.push(ModelInfo {
                id: routing_id,
                name: model.name.clone(),
                provider: routing_provider,
                context_window,
                tool_call: model.tool_call,
                reasoning: model.reasoning,
                input_audio,
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

    let document = tokio::fs::read_to_string(config_path)
        .await
        .ok()
        .and_then(|content| content.parse::<toml_edit::DocumentMut>().ok());

    let has_key = |key: &str, env_var: &str| {
        if let Some(doc) = document.as_ref()
            && let Some(llm) = doc.get("llm")
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

    if has_key("anthropic_key", "ANTHROPIC_API_KEY") {
        providers.push("anthropic");
    }
    if has_key("openai_key", "OPENAI_API_KEY") {
        providers.push("openai");
    }
    if config_path
        .parent()
        .is_some_and(|instance_dir| crate::openai_auth::credentials_path(instance_dir).exists())
    {
        providers.push("openai-chatgpt");
    }
    if has_key("openrouter_key", "OPENROUTER_API_KEY") {
        providers.push("openrouter");
    }
    if has_key("kilo_key", "KILO_API_KEY") {
        providers.push("kilo");
    }
    if has_key("zhipu_key", "ZHIPU_API_KEY") {
        providers.push("zhipu");
    }
    if has_key("groq_key", "GROQ_API_KEY") {
        providers.push("groq");
    }
    if has_key("together_key", "TOGETHER_API_KEY") {
        providers.push("together");
    }
    if has_key("fireworks_key", "FIREWORKS_API_KEY") {
        providers.push("fireworks");
    }
    if has_key("deepseek_key", "DEEPSEEK_API_KEY") {
        providers.push("deepseek");
    }
    if has_key("xai_key", "XAI_API_KEY") {
        providers.push("xai");
    }
    if has_key("mistral_key", "MISTRAL_API_KEY") {
        providers.push("mistral");
    }
    if has_key("gemini_key", "GEMINI_API_KEY") {
        providers.push("gemini");
    }
    if has_key("opencode_zen_key", "OPENCODE_ZEN_API_KEY") {
        providers.push("opencode-zen");
    }
    if has_key("opencode_go_key", "OPENCODE_GO_API_KEY") {
        providers.push("opencode-go");
    }
    if has_key("minimax_key", "MINIMAX_API_KEY") {
        providers.push("minimax");
    }
    if has_key("minimax_cn_key", "MINIMAX_CN_API_KEY") {
        providers.push("minimax-cn");
    }
    if has_key("moonshot_key", "MOONSHOT_API_KEY") {
        providers.push("moonshot");
    }
    if has_key("zai_coding_plan_key", "ZAI_CODING_PLAN_API_KEY") {
        providers.push("zai-coding-plan");
    }

    providers
}

pub(super) async fn get_models(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<ModelsQuery>,
) -> Result<Json<ModelsResponse>, StatusCode> {
    let config_path = state.config_path.read().await.clone();
    let configured = configured_providers(&config_path).await;
    let requested_provider = query
        .provider
        .as_deref()
        .map(str::trim)
        .filter(|provider| !provider.is_empty());
    let requested_provider_for_catalog = if requested_provider == Some("openai-chatgpt") {
        Some("openai")
    } else {
        requested_provider
    };
    let requested_capability = query
        .capability
        .as_deref()
        .map(str::trim)
        .filter(|capability| !capability.is_empty());

    let catalog = ensure_models_cache().await;
    let capability_matches = |model: &ModelInfo| {
        if let Some(capability) = requested_capability {
            match capability {
                "input_audio" => model.input_audio,
                "voice_transcription" => {
                    model.input_audio && is_known_voice_transcription_model(&model.id)
                }
                _ => true,
            }
        } else {
            true
        }
    };

    let mut models: Vec<ModelInfo> = catalog
        .iter()
        .filter(|model| {
            let provider_match = if let Some(provider) = requested_provider_for_catalog {
                model.provider == provider
            } else {
                configured.contains(&model.provider.as_str())
            };
            if !provider_match {
                return false;
            }
            capability_matches(model)
        })
        .cloned()
        .collect();

    if requested_provider == Some("openai-chatgpt") {
        models = models
            .into_iter()
            .filter_map(|model| as_openai_chatgpt_model(&model))
            .collect();
    } else if requested_provider.is_none() && configured.contains(&"openai-chatgpt") {
        let chatgpt_models: Vec<ModelInfo> = catalog
            .iter()
            .filter(|model| model.provider == "openai" && capability_matches(model))
            .filter_map(as_openai_chatgpt_model)
            .collect();
        models.extend(chatgpt_models);
    }

    for model in extra_models() {
        if let Some(capability) = requested_capability {
            if capability == "input_audio" && !model.input_audio {
                continue;
            }
            if capability == "voice_transcription"
                && (!model.input_audio || !is_known_voice_transcription_model(&model.id))
            {
                continue;
            }
        }
        if let Some(provider) = requested_provider {
            if model.provider == provider {
                models.push(model);
            }
        } else if configured.contains(&model.provider.as_str()) {
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

    get_models(
        State(state),
        Query(ModelsQuery {
            provider: None,
            capability: None,
        }),
    )
    .await
}
