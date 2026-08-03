use super::state::ApiState;

use axum::Json;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Serialize, Clone, utoipa::ToSchema)]
pub(super) struct ModelInfo {
    /// Full routing string (e.g. "openrouter/anthropic/claude-sonnet-4")
    pub(super) id: String,
    /// Human-readable name
    pub(super) name: String,
    /// Provider ID for routing ("anthropic", "openrouter", "openai", etc.)
    pub(super) provider: String,
    /// Context window size in tokens, if known
    pub(super) context_window: Option<u64>,
    /// Whether this model supports tool/function calling
    pub(super) tool_call: bool,
    /// Whether this model has reasoning/thinking capability
    pub(super) reasoning: bool,
    /// Whether this model accepts audio input.
    pub(super) input_audio: bool,
}

#[derive(Serialize, utoipa::ToSchema)]
pub(super) struct ModelsResponse {
    models: Vec<ModelInfo>,
}

#[derive(Deserialize, utoipa::ToSchema, utoipa::IntoParams)]
pub(super) struct ModelsQuery {
    provider: Option<String>,
    capability: Option<String>,
}

#[derive(Deserialize, utoipa::ToSchema)]
struct ModelsDevProvider {
    #[allow(dead_code)]
    id: Option<String>,
    #[allow(dead_code)]
    name: Option<String>,
    #[serde(default)]
    models: HashMap<String, ModelsDevModel>,
}

#[derive(Deserialize, utoipa::ToSchema)]
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

#[derive(Deserialize, utoipa::ToSchema)]
struct ModelsDevLimit {
    context: u64,
}

#[derive(Deserialize, utoipa::ToSchema)]
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

fn is_known_voice_transcription_model(model_id: &str) -> bool {
    KNOWN_VOICE_TRANSCRIPTION_MODELS.contains(&model_id)
}

/// Fetch the model catalog from models.dev.
///
/// Entries are stored *unprefixed* — `id` is the bare model name as the
/// upstream API expects it (`claude-sonnet-4`, `gpt-4.1`), and `provider` is
/// the models.dev provider the entry came from, used only for labelling.
///
/// Prefixing happens per configured provider in `get_models`, because a model
/// name is only meaningful relative to the endpoint being asked. The same
/// `claude-sonnet-4` string is valid against Anthropic directly, against a
/// LiteLLM alias, and against a self-hosted gateway — the catalog cannot know
/// which of your providers serves it, so it offers all of them.
async fn fetch_models_dev() -> anyhow::Result<Vec<ModelInfo>> {
    let client = reqwest::Client::new();
    let response = client
        .get("https://models.dev/api.json")
        .timeout(std::time::Duration::from_secs(15))
        .send()
        .await?
        .error_for_status()?;

    let catalog: HashMap<String, ModelsDevProvider> = response.json().await?;
    let mut seen: HashMap<String, ModelInfo> = HashMap::new();

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

            let input_audio = model
                .modalities
                .as_ref()
                .and_then(|m| m.input.as_ref())
                .is_some_and(|inputs| {
                    inputs
                        .iter()
                        .any(|input| input.to_lowercase().contains("audio"))
                });

            // The same model id can appear under several models.dev providers.
            // Keep the first, but let a later entry upgrade a capability flag —
            // understating capabilities hides working models from the picker.
            seen.entry(model_id.clone())
                .and_modify(|existing| {
                    existing.tool_call |= model.tool_call;
                    existing.reasoning |= model.reasoning;
                    existing.input_audio |= input_audio;
                })
                .or_insert_with(|| ModelInfo {
                    id: model_id.clone(),
                    name: model.name.clone(),
                    provider: provider_id.clone(),
                    context_window: model.limit.as_ref().map(|l| l.context),
                    tool_call: model.tool_call,
                    reasoning: model.reasoning,
                    input_audio,
                });
        }
    }

    let mut models: Vec<ModelInfo> = seen.into_values().collect();
    models.sort_by(|a, b| a.name.cmp(&b.name).then(a.id.cmp(&b.id)));

    Ok(models)
}

/// Ensure the cache is populated (fetches on first call, then uses TTL).
pub(super) async fn ensure_models_cache() -> Vec<ModelInfo> {
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

/// The providers this instance has configured, as `[llm.provider.<id>]` ids.
///
/// Reads the resolved config rather than re-parsing TOML by hand, so
/// env-var-bootstrapped providers (`ANTHROPIC_API_KEY`, `LITELLM_API_KEY`)
/// show up here too.
pub(super) async fn configured_providers(config_path: &std::path::Path) -> Vec<String> {
    let config_path = config_path.to_path_buf();
    let config = tokio::task::spawn_blocking(move || {
        crate::config::Config::load_from_path(&config_path).ok()
    })
    .await
    .ok()
    .flatten();

    let Some(config) = config else {
        return Vec::new();
    };

    let mut providers: Vec<String> = config.llm.providers.into_keys().collect();
    providers.sort();
    providers
}

#[utoipa::path(
    get,
    path = "/models",
    params(
        ("provider" = Option<String>, Query, description = "Filter by provider ID"),
        ("capability" = Option<String>, Query, description = "Filter by capability (input_audio, voice_transcription)"),
    ),
    responses(
        (status = 200, body = ModelsResponse),
        (status = 500, description = "Internal server error"),
    ),
    tag = "models",
)]
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
    let requested_capability = query
        .capability
        .as_deref()
        .map(str::trim)
        .filter(|capability| !capability.is_empty());

    // Offer the catalog under each configured provider id. A gateway can serve
    // any upstream model, so the picker cannot narrow this down for you — but
    // a routing string is only usable if its prefix is a provider you have.
    let target_providers: Vec<String> = match requested_provider {
        Some(provider) => vec![provider.to_string()],
        None => configured,
    };

    let catalog = ensure_models_cache().await;
    let mut models = Vec::new();

    for provider in &target_providers {
        for model in &catalog {
            let routing_id = format!("{provider}/{}", model.id);

            if let Some(capability) = requested_capability {
                let matches = match capability {
                    "input_audio" => model.input_audio,
                    "voice_transcription" => {
                        model.input_audio && is_known_voice_transcription_model(&routing_id)
                    }
                    _ => true,
                };
                if !matches {
                    continue;
                }
            }

            models.push(ModelInfo {
                id: routing_id,
                name: model.name.clone(),
                provider: provider.clone(),
                context_window: model.context_window,
                tool_call: model.tool_call,
                reasoning: model.reasoning,
                input_audio: model.input_audio,
            });
        }
    }

    models.sort_by(|a, b| a.provider.cmp(&b.provider).then(a.name.cmp(&b.name)));

    Ok(Json(ModelsResponse { models }))
}

#[utoipa::path(
    post,
    path = "/models/refresh",
    responses(
        (status = 200, body = ModelsResponse),
        (status = 500, description = "Internal server error"),
    ),
    tag = "models",
)]
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
