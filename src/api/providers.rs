use super::state::ApiState;

use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Serialize)]
pub(super) struct ProviderStatus {
    anthropic: bool,
    openai: bool,
    openrouter: bool,
    zhipu: bool,
    groq: bool,
    together: bool,
    fireworks: bool,
    deepseek: bool,
    xai: bool,
    mistral: bool,
    opencode_zen: bool,
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
}

#[derive(Serialize)]
pub(super) struct ProviderUpdateResponse {
    success: bool,
    message: String,
}

pub(super) async fn get_providers(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ProvidersResponse>, StatusCode> {
    let config_path = state.config_path.read().await.clone();

    let (anthropic, openai, openrouter, zhipu, groq, together, fireworks, deepseek, xai, mistral, opencode_zen) = if config_path.exists() {
        let content = tokio::fs::read_to_string(&config_path)
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        let doc: toml_edit::DocumentMut = content
            .parse()
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

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

        (
            has_key("anthropic_key", "ANTHROPIC_API_KEY"),
            has_key("openai_key", "OPENAI_API_KEY"),
            has_key("openrouter_key", "OPENROUTER_API_KEY"),
            has_key("zhipu_key", "ZHIPU_API_KEY"),
            has_key("groq_key", "GROQ_API_KEY"),
            has_key("together_key", "TOGETHER_API_KEY"),
            has_key("fireworks_key", "FIREWORKS_API_KEY"),
            has_key("deepseek_key", "DEEPSEEK_API_KEY"),
            has_key("xai_key", "XAI_API_KEY"),
            has_key("mistral_key", "MISTRAL_API_KEY"),
            has_key("opencode_zen_key", "OPENCODE_ZEN_API_KEY"),
        )
    } else {
        (
            std::env::var("ANTHROPIC_API_KEY").is_ok(),
            std::env::var("OPENAI_API_KEY").is_ok(),
            std::env::var("OPENROUTER_API_KEY").is_ok(),
            std::env::var("ZHIPU_API_KEY").is_ok(),
            std::env::var("GROQ_API_KEY").is_ok(),
            std::env::var("TOGETHER_API_KEY").is_ok(),
            std::env::var("FIREWORKS_API_KEY").is_ok(),
            std::env::var("DEEPSEEK_API_KEY").is_ok(),
            std::env::var("XAI_API_KEY").is_ok(),
            std::env::var("MISTRAL_API_KEY").is_ok(),
            std::env::var("OPENCODE_ZEN_API_KEY").is_ok(),
        )
    };

    let providers = ProviderStatus {
        anthropic,
        openai,
        openrouter,
        zhipu,
        groq,
        together,
        fireworks,
        deepseek,
        xai,
        mistral,
        opencode_zen,
    };
    let has_any = providers.anthropic
        || providers.openai
        || providers.openrouter
        || providers.zhipu
        || providers.groq
        || providers.together
        || providers.fireworks
        || providers.deepseek
        || providers.xai
        || providers.mistral
        || providers.opencode_zen;

    Ok(Json(ProvidersResponse { providers, has_any }))
}

pub(super) async fn update_provider(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<ProviderUpdateRequest>,
) -> Result<Json<ProviderUpdateResponse>, StatusCode> {
    let key_name = match request.provider.as_str() {
        "anthropic" => "anthropic_key",
        "openai" => "openai_key",
        "openrouter" => "openrouter_key",
        "zhipu" => "zhipu_key",
        "groq" => "groq_key",
        "together" => "together_key",
        "fireworks" => "fireworks_key",
        "deepseek" => "deepseek_key",
        "xai" => "xai_key",
        "mistral" => "mistral_key",
        "opencode-zen" => "opencode_zen_key",
        _ => {
            return Ok(Json(ProviderUpdateResponse {
                success: false,
                message: format!("Unknown provider: {}", request.provider),
            }));
        }
    };

    if request.api_key.trim().is_empty() {
        return Ok(Json(ProviderUpdateResponse {
            success: false,
            message: "API key cannot be empty".into(),
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

    // Auto-set routing defaults if the current routing points to a provider
    // the user doesn't have a key for.
    let should_set_routing = {
        let current_channel = doc
            .get("defaults")
            .and_then(|d| d.get("routing"))
            .and_then(|r| r.get("channel"))
            .and_then(|v| v.as_str())
            .unwrap_or("anthropic/claude-sonnet-4-20250514");

        let current_provider = crate::llm::routing::provider_from_model(current_channel);

        let has_provider_key = |toml_key: &str, env_var: &str| -> bool {
            if let Some(s) = doc.get("llm").and_then(|l| l.get(toml_key)).and_then(|v| v.as_str()) {
                if let Some(var_name) = s.strip_prefix("env:") {
                    return std::env::var(var_name).is_ok();
                }
                return !s.is_empty();
            }
            std::env::var(env_var).is_ok()
        };

        let has_key_for_current = match current_provider {
            "anthropic" => has_provider_key("anthropic_key", "ANTHROPIC_API_KEY"),
            "openai" => has_provider_key("openai_key", "OPENAI_API_KEY"),
            "openrouter" => has_provider_key("openrouter_key", "OPENROUTER_API_KEY"),
            "zhipu" => has_provider_key("zhipu_key", "ZHIPU_API_KEY"),
            "groq" => has_provider_key("groq_key", "GROQ_API_KEY"),
            "together" => has_provider_key("together_key", "TOGETHER_API_KEY"),
            "fireworks" => has_provider_key("fireworks_key", "FIREWORKS_API_KEY"),
            "deepseek" => has_provider_key("deepseek_key", "DEEPSEEK_API_KEY"),
            "xai" => has_provider_key("xai_key", "XAI_API_KEY"),
            "mistral" => has_provider_key("mistral_key", "MISTRAL_API_KEY"),
            "opencode-zen" => has_provider_key("opencode_zen_key", "OPENCODE_ZEN_API_KEY"),
            _ => false,
        };

        !has_key_for_current
    };

    if should_set_routing {
        let routing = crate::llm::routing::defaults_for_provider(&request.provider);

        if doc.get("defaults").is_none() {
            doc["defaults"] = toml_edit::Item::Table(toml_edit::Table::new());
        }

        if let Some(defaults) = doc.get_mut("defaults").and_then(|d| d.as_table_mut()) {
            if defaults.get("routing").is_none() {
                defaults["routing"] = toml_edit::Item::Table(toml_edit::Table::new());
            }

            if let Some(routing_table) = defaults.get_mut("routing").and_then(|r| r.as_table_mut()) {
                routing_table["channel"] = toml_edit::value(&routing.channel);
                routing_table["branch"] = toml_edit::value(&routing.branch);
                routing_table["worker"] = toml_edit::value(&routing.worker);
                routing_table["compactor"] = toml_edit::value(&routing.compactor);
                routing_table["cortex"] = toml_edit::value(&routing.cortex);
            }
        }
    }

    tokio::fs::write(&config_path, doc.to_string())
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    state
        .provider_setup_tx
        .try_send(crate::ProviderSetupEvent::ProvidersConfigured)
        .ok();

    let routing_note = if should_set_routing {
        format!(" Model routing updated to use {} defaults.", request.provider)
    } else {
        String::new()
    };

    Ok(Json(ProviderUpdateResponse {
        success: true,
        message: format!("Provider '{}' configured.{}", request.provider, routing_note),
    }))
}

pub(super) async fn delete_provider(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path(provider): axum::extract::Path<String>,
) -> Result<Json<ProviderUpdateResponse>, StatusCode> {
    let key_name = match provider.as_str() {
        "anthropic" => "anthropic_key",
        "openai" => "openai_key",
        "openrouter" => "openrouter_key",
        "zhipu" => "zhipu_key",
        "groq" => "groq_key",
        "together" => "together_key",
        "fireworks" => "fireworks_key",
        "deepseek" => "deepseek_key",
        "xai" => "xai_key",
        "mistral" => "mistral_key",
        "opencode-zen" => "opencode_zen_key",
        _ => {
            return Ok(Json(ProviderUpdateResponse {
                success: false,
                message: format!("Unknown provider: {}", provider),
            }));
        }
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

    if let Some(llm) = doc.get_mut("llm") {
        if let Some(table) = llm.as_table_mut() {
            table.remove(key_name);
        }
    }

    tokio::fs::write(&config_path, doc.to_string())
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(Json(ProviderUpdateResponse {
        success: true,
        message: format!("Provider '{}' removed", provider),
    }))
}
