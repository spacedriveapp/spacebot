//! TTS proxy: streams audio from Voicebox for voice output.

use super::state::ApiState;

use axum::Json;
use axum::body::Body;
use axum::extract::State;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Deserialize)]
pub(super) struct TtsGenerateRequest {
    /// The text to synthesize into speech.
    text: String,
    /// Voicebox voice profile ID. Uses the default profile if omitted.
    profile_id: Option<String>,
    /// TTS engine override (e.g. "chatterbox_turbo", "luxtts").
    engine: Option<String>,
    /// Target agent ID for resolving the Voicebox URL from config.
    agent_id: Option<String>,
}

#[derive(Deserialize, Serialize)]
pub(super) struct VoiceboxProfile {
    id: String,
    name: Option<String>,
    default_engine: Option<String>,
    preset_engine: Option<String>,
    effects_chain: Option<serde_json::Value>,
}

/// Proxy a TTS request to Voicebox's `/generate/stream` endpoint and stream
/// the resulting WAV audio bytes back to the caller.
///
/// The Voicebox URL is resolved from `SPACEBOT_VOICEBOX_URL` env or the
/// agent's `voicebox_url` runtime config field. Defaults to
/// `http://localhost:17493` if not configured.
pub(super) async fn tts_generate(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<TtsGenerateRequest>,
) -> Result<Response, StatusCode> {
    tracing::info!(agent_id = ?request.agent_id, profile_id = ?request.profile_id, engine = ?request.engine, "tts_generate request received");
    if request.text.trim().is_empty() {
        return Err(StatusCode::BAD_REQUEST);
    }

    // Resolve the Voicebox URL — check agent config first, then env default
    let voicebox_url = resolve_voicebox_url(&state, request.agent_id.as_deref());
    if voicebox_url.is_empty() {
        tracing::warn!("TTS requested but no voicebox_url configured");
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    }

    let endpoint = format!("{}/generate/stream", voicebox_url.trim_end_matches('/'));
    tracing::info!(%endpoint, "tts_generate proxying to Voicebox");

    // TTS generation can legitimately take longer than 10 seconds on the first
    // request after a backend/model restart, especially for cloned voices.
    // Use only a short connect timeout; leave the request itself unbounded.
    let client = reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(3))
        .build()
        .map_err(|error| {
            tracing::warn!(%error, "failed to build Voicebox HTTP client");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    // Voicebox requires profile_id. Resolve in this order:
    // request payload -> env default -> first profile from Voicebox.
    let profile_id = if let Some(profile_id) = request.profile_id.clone() {
        Some(profile_id)
    } else if let Ok(env_profile_id) = std::env::var("SPACEBOT_VOICEBOX_PROFILE_ID") {
        if env_profile_id.trim().is_empty() {
            None
        } else {
            Some(env_profile_id)
        }
    } else {
        resolve_default_profile_id(&client, &voicebox_url).await
    };

    let Some(profile_id) = profile_id else {
        tracing::warn!("Voicebox profile_id missing and no profiles available");
        return Err(StatusCode::UNPROCESSABLE_ENTITY);
    };

    let selected_profile = fetch_profile(&client, &voicebox_url, &profile_id).await;
    let resolved_engine = request
        .engine
        .clone()
        .or_else(|| {
            selected_profile
                .as_ref()
                .and_then(|p| p.default_engine.clone())
        })
        .or_else(|| {
            selected_profile
                .as_ref()
                .and_then(|p| p.preset_engine.clone())
        });

    let mut body = serde_json::json!({
        "text": request.text,
        "profile_id": profile_id,
    });

    if let Some(engine) = resolved_engine {
        body["engine"] = serde_json::Value::String(engine);
    }
    if let Some(effects_chain) = selected_profile.and_then(|p| p.effects_chain) {
        body["effects_chain"] = effects_chain;
    }

    // Stream the response from Voicebox back to the client
    let voicebox_response = client
        .post(&endpoint)
        .json(&body)
        .send()
        .await
        .map_err(|error| {
            tracing::warn!(%error, %endpoint, "failed to reach Voicebox for TTS");
            StatusCode::BAD_GATEWAY
        })?;

    if !voicebox_response.status().is_success() {
        let status = voicebox_response.status();
        tracing::warn!(
            voicebox_status = %status,
            "Voicebox returned error for TTS generate"
        );
        return Err(StatusCode::BAD_GATEWAY);
    }

    // Get content type from Voicebox (typically audio/wav)
    let content_type = voicebox_response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("audio/wav")
        .to_string();

    // Convert the reqwest streaming body into an axum streaming response
    let stream = voicebox_response.bytes_stream();
    let body = Body::from_stream(stream);

    Ok(Response::builder()
        .status(200)
        .header(header::CONTENT_TYPE, content_type)
        .header(header::CACHE_CONTROL, "no-cache")
        .body(body)
        .unwrap()
        .into_response())
}

/// List Voicebox profiles so the desktop voice UI can let the user choose
/// which voice to speak with.
pub(super) async fn tts_profiles(
    State(state): State<Arc<ApiState>>,
    axum::extract::Query(query): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Result<Json<Vec<VoiceboxProfile>>, StatusCode> {
    let voicebox_url = resolve_voicebox_url(&state, query.get("agent_id").map(String::as_str));
    let endpoint = format!("{}/profiles", voicebox_url.trim_end_matches('/'));
    tracing::info!(%endpoint, "tts_profiles proxying to Voicebox");
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|error| {
            tracing::warn!(%error, "failed to build Voicebox HTTP client");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;
    let response = client.get(endpoint).send().await.map_err(|error| {
        tracing::warn!(%error, "failed to reach Voicebox profiles endpoint");
        StatusCode::BAD_GATEWAY
    })?;
    if !response.status().is_success() {
        tracing::warn!(status = %response.status(), "Voicebox profiles returned error");
        return Err(StatusCode::BAD_GATEWAY);
    }
    let profiles = response
        .json::<Vec<VoiceboxProfile>>()
        .await
        .map_err(|error| {
            tracing::warn!(%error, "invalid Voicebox profiles response");
            StatusCode::BAD_GATEWAY
        })?;
    Ok(Json(profiles))
}

fn resolve_voicebox_url(state: &ApiState, agent_id: Option<&str>) -> String {
    // Try agent-specific config first
    if let Some(agent_id) = agent_id {
        let configs = state.runtime_configs.load();
        if let Some(config) = configs.get(agent_id) {
            let url = config.voicebox_url.load();
            if !url.is_empty() {
                return url.to_string();
            }
        }
    }

    // Fall back to env var, then localhost default.
    std::env::var("SPACEBOT_VOICEBOX_URL").unwrap_or_else(|_| "http://127.0.0.1:17493".to_string())
}

async fn resolve_default_profile_id(
    client: &reqwest::Client,
    voicebox_url: &str,
) -> Option<String> {
    let endpoint = format!("{}/profiles", voicebox_url.trim_end_matches('/'));
    let response = client.get(endpoint).send().await.ok()?;
    if !response.status().is_success() {
        return None;
    }
    let profiles = response.json::<Vec<VoiceboxProfile>>().await.ok()?;
    profiles.first().map(|p| p.id.clone())
}

async fn fetch_profile(
    client: &reqwest::Client,
    voicebox_url: &str,
    profile_id: &str,
) -> Option<VoiceboxProfile> {
    let endpoint = format!("{}/profiles", voicebox_url.trim_end_matches('/'));
    let response = client.get(endpoint).send().await.ok()?;
    if !response.status().is_success() {
        return None;
    }
    let profiles = response.json::<Vec<VoiceboxProfile>>().await.ok()?;
    profiles
        .into_iter()
        .find(|profile| profile.id == profile_id)
}
