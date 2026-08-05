//! Provider configuration endpoints.
//!
//! A provider is a `[llm.provider.<id>]` block: an `api_type` (`anthropic` or
//! `openai_compatible`), a `base_url`, and an `api_key`. That is the whole
//! model. There is no per-vendor endpoint here because there are no per-vendor
//! providers — adding OpenRouter, Groq, or a self-hosted vLLM is the same POST
//! with a different `base_url`.

use super::state::ApiState;

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use rig::agent::AgentBuilder;
use rig::completion::{CompletionModel as _, Prompt as _};
use serde::{Deserialize, Serialize};

use std::collections::HashMap;
use std::sync::Arc;

/// A configured provider, as reported to the UI. Never includes the API key.
#[derive(Serialize, utoipa::ToSchema)]
pub(super) struct ProviderEntry {
    /// Provider id — the prefix in `provider/model` routing strings.
    id: String,
    /// `"anthropic"` or `"openai_compatible"`.
    api_type: String,
    base_url: String,
    /// Optional human-readable label from `name`.
    display_name: Option<String>,
    /// Whether an API key resolves for this provider. False means the block
    /// exists but its `secret:`/`env:` reference is unresolvable.
    has_key: bool,
}

#[derive(Serialize, utoipa::ToSchema)]
pub(super) struct ProvidersResponse {
    providers: Vec<ProviderEntry>,
    /// Whether Anthropic OAuth credentials are on disk (`spacebot auth login`).
    /// This authenticates the `anthropic` provider without an API key.
    anthropic_oauth: bool,
    has_any: bool,
}

#[derive(Deserialize, utoipa::ToSchema)]
pub(super) struct ProviderUpdateRequest {
    /// Provider id to create or replace, e.g. `"litellm"`.
    provider: String,
    api_key: String,
    /// Routing string to apply to defaults and the default agent, e.g.
    /// `"litellm/claude-sonnet-4"`. Must be prefixed with `provider`.
    model: String,
    /// `"anthropic"` or `"openai_compatible"`. Defaults to `openai_compatible`.
    #[serde(default)]
    api_type: Option<String>,
    /// Full path prefix. Required unless `api_type` is `anthropic`.
    #[serde(default)]
    base_url: Option<String>,
}

#[derive(Serialize, utoipa::ToSchema)]
pub(super) struct ProviderUpdateResponse {
    success: bool,
    message: String,
}

#[derive(Deserialize, utoipa::ToSchema)]
pub(super) struct ProviderModelTestRequest {
    provider: String,
    api_key: String,
    model: String,
    #[serde(default)]
    api_type: Option<String>,
    #[serde(default)]
    base_url: Option<String>,
}

#[derive(Serialize, utoipa::ToSchema)]
pub(super) struct ProviderModelTestResponse {
    success: bool,
    message: String,
    provider: String,
    model: String,
    sample: Option<String>,
}

/// Provider ids must be usable as a routing prefix and a TOML key.
fn validate_provider_id(provider: &str) -> Result<(), String> {
    if provider.is_empty() || provider.len() > 64 {
        return Err("Provider ID must be between 1 and 64 characters".to_string());
    }
    if provider.contains('/') || provider.contains(char::is_whitespace) {
        return Err("Provider ID cannot contain '/' or whitespace".to_string());
    }
    Ok(())
}

/// Parse the `api_type` field from a request into the two supported values.
fn parse_api_type(api_type: Option<&str>) -> Result<crate::config::ApiType, String> {
    match api_type.map(str::trim).filter(|value| !value.is_empty()) {
        None | Some("openai_compatible") => Ok(crate::config::ApiType::OpenAiCompatible),
        Some("anthropic") => Ok(crate::config::ApiType::Anthropic),
        Some(other) => Err(format!(
            "Unknown api_type '{other}'. Use \"anthropic\" or \"openai_compatible\"."
        )),
    }
}

/// Resolve the `base_url` for a provider, defaulting only for Anthropic.
///
/// An OpenAI-compatible provider without a base URL is not a provider, so this
/// refuses rather than guessing an endpoint that would 404 later.
fn resolve_base_url(
    api_type: crate::config::ApiType,
    base_url: Option<&str>,
) -> Result<String, String> {
    let supplied = base_url.map(str::trim).filter(|value| !value.is_empty());

    match (api_type, supplied) {
        (_, Some(url)) => {
            reqwest::Url::parse(url)
                .map_err(|error| format!("Invalid base_url '{url}': {error}"))?;
            Ok(url.trim_end_matches('/').to_string())
        }
        (crate::config::ApiType::Anthropic, None) => {
            Ok(crate::config::ANTHROPIC_PROVIDER_BASE_URL.to_string())
        }
        (crate::config::ApiType::OpenAiCompatible, None) => Err(
            "base_url is required for openai_compatible providers. It is the full path \
             prefix, e.g. \"http://localhost:4000/v1\"."
                .to_string(),
        ),
    }
}

fn model_matches_provider(provider: &str, model: &str) -> bool {
    crate::llm::routing::provider_from_model(model) == provider
}

/// Resolve the credential and endpoint for a model test.
///
/// A caller-supplied `base_url` is honored only when the caller also supplied
/// the key. When the key comes from stored config, the endpoint is whatever
/// the provider is configured with — sending the stored credential to a
/// caller-chosen URL would hand the real key to any server an API-token
/// holder points the test at.
fn resolve_test_target(
    api_type: crate::config::ApiType,
    request_api_key: &str,
    request_base_url: Option<&str>,
    stored: Option<&crate::config::ProviderConfig>,
) -> Result<(String, String), String> {
    let caller_key = request_api_key.trim();
    let caller_url = request_base_url
        .map(str::trim)
        .filter(|value| !value.is_empty());

    if caller_key.is_empty() {
        let stored = stored
            .filter(|provider| !provider.api_key.trim().is_empty())
            .ok_or_else(|| "API key is required but not provided".to_string())?;
        Ok((stored.api_key.trim().to_string(), stored.base_url.clone()))
    } else {
        let base_url = match caller_url {
            Some(url) => resolve_base_url(api_type, Some(url))?,
            None => match stored {
                Some(provider) => provider.base_url.clone(),
                None => resolve_base_url(api_type, None)?,
            },
        };
        Ok((caller_key.to_string(), base_url))
    }
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

/// Build a throwaway `LlmConfig` holding exactly the provider under test.
fn build_test_llm_config(
    provider: &str,
    api_type: crate::config::ApiType,
    base_url: String,
    credential: &str,
) -> crate::config::LlmConfig {
    let mut providers = HashMap::new();
    providers.insert(
        provider.to_string(),
        crate::config::ProviderConfig {
            api_type,
            base_url,
            api_key: credential.to_string(),
            name: None,
            use_bearer_auth: false,
            extra_headers: Vec::new(),
        },
    );

    crate::config::LlmConfig { providers }
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

/// Environment variables that bootstrap a provider even when config.toml does
/// not define it (`Config::load_from_env` / `Config::load_from_path`). Deleting
/// such a provider from the file would silently reappear on next load, so the
/// delete handler refuses while the variable is set.
fn env_bootstrap_vars(provider_id: &str) -> &'static [&'static str] {
    match provider_id {
        "anthropic" => &["ANTHROPIC_API_KEY", "ANTHROPIC_AUTH_TOKEN"],
        "litellm" => &["LITELLM_API_KEY"],
        _ => &[],
    }
}

/// Drop model-valued entries whose provider prefix matches from one routing
/// table. The keys mirror what `apply_model_routing` writes.
fn scrub_routing_table(routing: &mut toml_edit::Table, provider_id: &str) {
    const MODEL_KEYS: &[&str] = &[
        "channel",
        "branch",
        "worker",
        "compactor",
        "cortex",
        "voice",
    ];
    for key in MODEL_KEYS {
        let dangling = routing
            .get(key)
            .and_then(|item| item.as_str())
            .is_some_and(|model| crate::llm::routing::provider_from_model(model) == provider_id);
        if dangling {
            routing.remove(key);
        }
    }
}

/// Remove routing entries pointing at a deleted provider from
/// `[defaults.routing]` and every agent's routing table, so the deleted
/// provider does not leave dangling `provider/model` strings behind. A table
/// left empty is removed entirely so config loading re-infers routing from
/// the remaining providers instead of falling back to the hardcoded default.
fn scrub_provider_routing(doc: &mut toml_edit::DocumentMut, provider_id: &str) {
    if let Some(defaults) = doc.get_mut("defaults").and_then(|item| item.as_table_mut()) {
        let now_empty = match defaults
            .get_mut("routing")
            .and_then(|item| item.as_table_mut())
        {
            Some(routing) => {
                scrub_routing_table(routing, provider_id);
                routing.is_empty()
            }
            None => false,
        };
        if now_empty {
            defaults.remove("routing");
        }
    }

    if let Some(agents) = doc
        .get_mut("agents")
        .and_then(|item| item.as_array_of_tables_mut())
    {
        for agent in agents.iter_mut() {
            let now_empty = match agent
                .get_mut("routing")
                .and_then(|item| item.as_table_mut())
            {
                Some(routing) => {
                    scrub_routing_table(routing, provider_id);
                    routing.is_empty()
                }
                None => false,
            };
            if now_empty {
                agent.remove("routing");
            }
        }
    }
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
    let anthropic_oauth = crate::auth::credentials_path(&instance_dir).exists();

    // Read straight from the parsed config so the API reports what the daemon
    // actually resolved — including env-var bootstrapped providers that never
    // appear in config.toml.
    let providers = match crate::config::Config::load_from_path(&config_path) {
        Ok(config) => {
            let mut entries: Vec<ProviderEntry> = config
                .llm
                .providers
                .into_iter()
                .map(|(id, provider)| ProviderEntry {
                    id,
                    api_type: provider.api_type.as_str().to_string(),
                    base_url: provider.base_url,
                    display_name: provider.name,
                    has_key: !provider.api_key.trim().is_empty(),
                })
                .collect();
            entries.sort_by(|a, b| a.id.cmp(&b.id));
            entries
        }
        Err(error) => {
            // A config that no longer parses (e.g. still using retired keys)
            // must not take down the settings UI — that is where the user goes
            // to fix it.
            tracing::warn!(%error, "failed to load config for provider listing");
            Vec::new()
        }
    };

    let has_any = !providers.is_empty() || anthropic_oauth;

    Ok(Json(ProvidersResponse {
        providers,
        anthropic_oauth,
        has_any,
    }))
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
    let provider_id = request.provider.trim().to_lowercase();
    let normalized_model = request.model.trim().to_string();

    let reject = |message: String| {
        Ok(Json(ProviderUpdateResponse {
            success: false,
            message,
        }))
    };

    if let Err(message) = validate_provider_id(&provider_id) {
        return reject(message);
    }

    let api_type = match parse_api_type(request.api_type.as_deref()) {
        Ok(api_type) => api_type,
        Err(message) => return reject(message),
    };

    let base_url = match resolve_base_url(api_type, request.base_url.as_deref()) {
        Ok(base_url) => base_url,
        Err(message) => return reject(message),
    };

    if request.api_key.trim().is_empty() {
        return reject("API key cannot be empty".into());
    }

    if normalized_model.is_empty() {
        return reject("Model cannot be empty".into());
    }

    if !model_matches_provider(&provider_id, &normalized_model) {
        return reject(format!(
            "Model '{normalized_model}' must be prefixed with the provider id, \
             e.g. '{provider_id}/{normalized_model}'."
        ));
    }

    let config_path = state.config_path.read().await.clone();

    // Serialize the config.toml read-modify-write with every other handler
    // that edits it; without the guard a concurrent write loses whole updates.
    let _config_guard = state.config_write_mutex.lock().await;

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
    if doc["llm"].get("provider").is_none() {
        doc["llm"]["provider"] = toml_edit::Item::Table(toml_edit::Table::new());
    }

    doc["llm"]["provider"][&provider_id]["api_type"] = toml_edit::value(api_type.as_str());
    doc["llm"]["provider"][&provider_id]["base_url"] = toml_edit::value(&base_url);
    doc["llm"]["provider"][&provider_id]["api_key"] = toml_edit::value(request.api_key.trim());

    apply_model_routing(&mut doc, &normalized_model);

    tokio::fs::write(&config_path, doc.to_string())
        .await
        .map_err(|error| {
            tracing::error!(%error, path = %config_path.display(), "failed to write config.toml for provider setup");
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
            "Provider '{provider_id}' configured. Model '{normalized_model}' applied to \
             defaults and the default agent routing."
        ),
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
    let provider_id = request.provider.trim().to_lowercase();
    let normalized_model = request.model.trim().to_string();

    let reject = |message: String| {
        Ok(Json(ProviderModelTestResponse {
            success: false,
            message,
            provider: request.provider.clone(),
            model: request.model.clone(),
            sample: None,
        }))
    };

    if let Err(message) = validate_provider_id(&provider_id) {
        return reject(message);
    }

    let api_type = match parse_api_type(request.api_type.as_deref()) {
        Ok(api_type) => api_type,
        Err(message) => return reject(message),
    };

    if normalized_model.is_empty() {
        return reject("Model cannot be empty".to_string());
    }

    if !model_matches_provider(&provider_id, &normalized_model) {
        return reject(format!(
            "Model '{normalized_model}' must be prefixed with the provider id, \
             e.g. '{provider_id}/{normalized_model}'."
        ));
    }

    // An empty key means "test what is already configured" — used by the UI to
    // re-check a provider without asking the user to retype a secret.
    let caller_url_supplied = request
        .base_url
        .as_deref()
        .is_some_and(|value| !value.trim().is_empty());
    let stored = if request.api_key.trim().is_empty() || !caller_url_supplied {
        let config_path = state.config_path.read().await.clone();
        crate::config::Config::load_from_path(&config_path)
            .ok()
            .and_then(|config| config.llm.providers.get(&provider_id).cloned())
    } else {
        None
    };

    let (api_key, base_url) = match resolve_test_target(
        api_type,
        &request.api_key,
        request.base_url.as_deref(),
        stored.as_ref(),
    ) {
        Ok(target) => target,
        Err(message) => return reject(message),
    };

    let llm_config = build_test_llm_config(&provider_id, api_type, base_url, &api_key);
    let llm_manager = match crate::llm::LlmManager::new(llm_config).await {
        Ok(manager) => Arc::new(manager),
        Err(error) => return reject(format!("Failed to initialize provider: {error}")),
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
        ("provider" = String, Path, description = "Provider ID to delete"),
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
    let provider_id = provider.trim().to_lowercase();

    // `anthropic-oauth` is not a config block — it is the credentials file
    // written by `spacebot auth login`.
    if provider_id == "anthropic-oauth" {
        let instance_dir = (**state.instance_dir.load()).clone();
        let credentials_path = crate::auth::credentials_path(&instance_dir);
        if credentials_path.exists() {
            tokio::fs::remove_file(&credentials_path).await.map_err(|error| {
                tracing::error!(%error, path = %credentials_path.display(), "failed to remove Anthropic OAuth credentials");
                StatusCode::INTERNAL_SERVER_ERROR
            })?;
        }
        return Ok(Json(ProviderUpdateResponse {
            success: true,
            message: "Anthropic OAuth credentials removed".into(),
        }));
    }

    // Env-bootstrapped providers are not owned by config.toml: removing the
    // file entry (if any) would not remove the provider, and it would reappear
    // on next load while the delete reported success.
    let bootstrap_vars: Vec<&str> = env_bootstrap_vars(&provider_id)
        .iter()
        .copied()
        .filter(|var| std::env::var(var).is_ok())
        .collect();
    if !bootstrap_vars.is_empty() {
        return Ok(Json(ProviderUpdateResponse {
            success: false,
            message: format!(
                "Provider '{provider_id}' is configured by the {} environment {}; \
                 unset {} to remove it.",
                bootstrap_vars.join(" / "),
                if bootstrap_vars.len() == 1 {
                    "variable"
                } else {
                    "variables"
                },
                bootstrap_vars.join(" / "),
            ),
        }));
    }

    let config_path = state.config_path.read().await.clone();
    if !config_path.exists() {
        return Ok(Json(ProviderUpdateResponse {
            success: false,
            message: "No config file found".into(),
        }));
    }

    // Serialize the config.toml read-modify-write with every other handler
    // that edits it; without the guard a concurrent write loses whole updates.
    let _config_guard = state.config_write_mutex.lock().await;

    let content = tokio::fs::read_to_string(&config_path).await.map_err(|error| {
        tracing::error!(%error, path = %config_path.display(), "failed to read config.toml for provider removal");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let mut doc: toml_edit::DocumentMut = content.parse().map_err(|error| {
        tracing::error!(%error, "failed to parse config.toml for provider removal");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let mut removed = false;
    if let Some(llm_table) = doc.get_mut("llm").and_then(|llm| llm.as_table_mut())
        && let Some(provider_table) = llm_table
            .get_mut("provider")
            .and_then(|item| item.as_table_mut())
    {
        removed = provider_table.remove(&provider_id).is_some();
        if provider_table.is_empty() {
            llm_table.remove("provider");
        }
    }

    if !removed {
        return Ok(Json(ProviderUpdateResponse {
            success: false,
            message: format!("Provider '{provider_id}' is not configured in config.toml"),
        }));
    }

    scrub_provider_routing(&mut doc, &provider_id);

    tokio::fs::write(&config_path, doc.to_string())
        .await
        .map_err(|error| {
            tracing::error!(%error, path = %config_path.display(), "failed to write config.toml for provider removal");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    refresh_defaults_config(&state).await;

    Ok(Json(ProviderUpdateResponse {
        success: true,
        message: format!("Provider '{provider_id}' removed"),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ApiType;

    #[test]
    fn parse_api_type_defaults_to_openai_compatible() {
        assert_eq!(parse_api_type(None).unwrap(), ApiType::OpenAiCompatible);
        assert_eq!(parse_api_type(Some("")).unwrap(), ApiType::OpenAiCompatible);
        assert_eq!(
            parse_api_type(Some("openai_compatible")).unwrap(),
            ApiType::OpenAiCompatible
        );
        assert_eq!(
            parse_api_type(Some("anthropic")).unwrap(),
            ApiType::Anthropic
        );
    }

    #[test]
    fn parse_api_type_rejects_retired_names() {
        for retired in ["azure", "gemini", "kilo_gateway", "openai_responses"] {
            assert!(
                parse_api_type(Some(retired)).is_err(),
                "{retired} should be rejected"
            );
        }
    }

    #[test]
    fn openai_compatible_requires_an_explicit_base_url() {
        assert!(resolve_base_url(ApiType::OpenAiCompatible, None).is_err());
        assert_eq!(
            resolve_base_url(ApiType::OpenAiCompatible, Some("http://localhost:4000/v1/")).unwrap(),
            "http://localhost:4000/v1"
        );
    }

    #[test]
    fn anthropic_falls_back_to_the_public_endpoint() {
        assert_eq!(
            resolve_base_url(ApiType::Anthropic, None).unwrap(),
            "https://api.anthropic.com"
        );
    }

    #[test]
    fn provider_ids_cannot_break_routing_strings() {
        assert!(validate_provider_id("litellm").is_ok());
        assert!(validate_provider_id("").is_err());
        assert!(validate_provider_id("has/slash").is_err());
        assert!(validate_provider_id("has space").is_err());
    }

    fn stored_provider() -> crate::config::ProviderConfig {
        crate::config::ProviderConfig {
            api_type: ApiType::OpenAiCompatible,
            base_url: "http://localhost:4000/v1".to_string(),
            api_key: "sk-real".to_string(),
            name: None,
            use_bearer_auth: false,
            extra_headers: Vec::new(),
        }
    }

    #[test]
    fn test_target_with_stored_key_ignores_caller_base_url() {
        let stored = stored_provider();
        let (api_key, base_url) = resolve_test_target(
            ApiType::OpenAiCompatible,
            "",
            Some("http://attacker.example.com/v1"),
            Some(&stored),
        )
        .unwrap();
        assert_eq!(api_key, "sk-real");
        assert_eq!(base_url, "http://localhost:4000/v1");
    }

    #[test]
    fn test_target_with_caller_key_honors_caller_base_url() {
        let (api_key, base_url) = resolve_test_target(
            ApiType::OpenAiCompatible,
            "sk-caller",
            Some("http://localhost:9999/v1/"),
            None,
        )
        .unwrap();
        assert_eq!(api_key, "sk-caller");
        assert_eq!(base_url, "http://localhost:9999/v1");
    }

    #[test]
    fn test_target_with_caller_key_falls_back_to_stored_base_url() {
        let stored = stored_provider();
        let (api_key, base_url) =
            resolve_test_target(ApiType::OpenAiCompatible, "sk-caller", None, Some(&stored))
                .unwrap();
        assert_eq!(api_key, "sk-caller");
        assert_eq!(base_url, "http://localhost:4000/v1");
    }

    #[test]
    fn test_target_requires_a_key_when_nothing_is_stored() {
        assert!(resolve_test_target(ApiType::Anthropic, "", None, None).is_err());
        let mut stored = stored_provider();
        stored.api_key = "  ".to_string();
        assert!(resolve_test_target(ApiType::OpenAiCompatible, "", None, Some(&stored)).is_err());
    }

    #[test]
    fn env_bootstrap_vars_cover_only_the_bootstrapped_providers() {
        assert_eq!(
            env_bootstrap_vars("anthropic"),
            &["ANTHROPIC_API_KEY", "ANTHROPIC_AUTH_TOKEN"]
        );
        assert_eq!(env_bootstrap_vars("litellm"), &["LITELLM_API_KEY"]);
        assert!(env_bootstrap_vars("openrouter").is_empty());
    }

    #[test]
    fn scrub_provider_routing_removes_only_dangling_entries() {
        let mut doc: toml_edit::DocumentMut = r#"
[defaults.routing]
channel = "litellm/claude-sonnet-4"
branch = "anthropic/claude-sonnet-4"
worker = "litellm/claude-sonnet-4"
rate_limit_cooldown_secs = 60

[[agents]]
id = "main"
default = true

[agents.routing]
channel = "litellm/claude-sonnet-4"
worker = "anthropic/claude-sonnet-4"
"#
        .parse()
        .unwrap();

        scrub_provider_routing(&mut doc, "litellm");

        let defaults_routing = doc["defaults"]["routing"].as_table().unwrap();
        assert!(defaults_routing.get("channel").is_none());
        assert!(defaults_routing.get("worker").is_none());
        assert_eq!(
            defaults_routing["branch"].as_str(),
            Some("anthropic/claude-sonnet-4")
        );

        let agents = doc["agents"].as_array_of_tables().unwrap();
        let agent_routing = agents.iter().next().unwrap();
        assert!(
            agent_routing["routing"]
                .as_table()
                .unwrap()
                .get("channel")
                .is_none()
        );
        assert_eq!(
            agent_routing["routing"]["worker"].as_str(),
            Some("anthropic/claude-sonnet-4")
        );
    }

    #[test]
    fn scrub_provider_routing_drops_a_table_left_empty() {
        let mut doc: toml_edit::DocumentMut =
            "[defaults.routing]\nchannel = \"litellm/claude-sonnet-4\"\n"
                .parse()
                .unwrap();

        scrub_provider_routing(&mut doc, "litellm");

        // Removing the emptied table lets config loading re-infer routing from
        // the remaining providers instead of keeping a dangling default.
        let routing = doc
            .get("defaults")
            .and_then(|defaults| defaults.get("routing"));
        assert!(routing.is_none());
    }
}
