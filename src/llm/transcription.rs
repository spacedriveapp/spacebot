//! Whisper-compatible audio transcription.
//!
//! Provides speech-to-text transcription using OpenAI-compatible /v1/audio/transcriptions
//! endpoints (OpenAI Whisper, Groq Whisper, Gemini OpenAI-compatible).

use crate::config::ProviderConfig;
use crate::error::{LlmError, Result};

/// Request for audio transcription.
pub struct TranscriptionRequest<'a> {
    /// Raw audio bytes.
    pub audio_bytes: &'a [u8],
    /// Original filename (used for MIME detection).
    pub filename: &'a str,
    /// MIME type of the audio.
    pub mime_type: &'a str,
    /// Model name (e.g., "whisper-1", "whisper-large-v3-turbo").
    pub model: &'a str,
    /// Optional language hint for accuracy (e.g., "en", "es").
    pub language: Option<&'a str>,
    /// If true, use /audio/translations endpoint (translates to English).
    pub translate: bool,
}

/// Response from audio transcription.
pub struct TranscriptionResponse {
    /// Transcribed text.
    pub text: String,
    /// Duration of the input audio in seconds (if provided by API).
    pub duration_secs: Option<f64>,
    /// True if translation mode was used.
    pub translated: bool,
}

/// Transcribe audio using a Whisper-compatible API.
///
/// Supports OpenAI, Groq, and Gemini OpenAI-compatible endpoints.
/// Uses multipart form data for the request.
pub async fn transcribe_audio(
    http: &reqwest::Client,
    provider: &ProviderConfig,
    request: TranscriptionRequest<'_>,
) -> Result<TranscriptionResponse> {
    transcribe_with_whisper_compatible(http, provider, request).await
}

async fn transcribe_with_whisper_compatible(
    http: &reqwest::Client,
    provider: &ProviderConfig,
    request: TranscriptionRequest<'_>,
) -> Result<TranscriptionResponse> {
    let endpoint = build_whisper_endpoint(&provider.base_url, request.translate);
    let form = build_multipart_form(&request)?;

    let mut request_builder = http
        .post(&endpoint)
        .header("Authorization", format!("Bearer {}", provider.api_key));

    for (key, value) in &provider.extra_headers {
        request_builder = request_builder.header(key, value);
    }

    let response = request_builder
        .multipart(form)
        .send()
        .await
        .map_err(|e| LlmError::ProviderRequest(format!("HTTP request failed: {}", e)))?;
    parse_whisper_response(response, request.translate).await
}

/// Build the Whisper API endpoint URL.
///
/// Provider-specific paths:
/// - Groq: /openai/v1/audio/{transcriptions,translations}
/// - OpenAI/Gemini: /v1/audio/{transcriptions,translations}
fn build_whisper_endpoint(base_url: &str, translate: bool) -> String {
    let base = base_url.trim_end_matches('/');
    let path = if translate {
        "audio/translations"
    } else {
        "audio/transcriptions"
    };

    if base.contains("groq.com") {
        format!("{}/openai/v1/{}", base, path)
    } else {
        format!("{}/v1/{}", base, path)
    }
}

/// Build a multipart form for the Whisper API request.
fn build_multipart_form(request: &TranscriptionRequest<'_>) -> Result<reqwest::multipart::Form> {
    let audio_part = reqwest::multipart::Part::bytes(request.audio_bytes.to_vec())
        .file_name(request.filename.to_string())
        .mime_str(request.mime_type)
        .map_err(|e| LlmError::ProviderRequest(format!("invalid MIME type: {}", e)))?;

    let mut form = reqwest::multipart::Form::new()
        .part("file", audio_part)
        .text("model", request.model.to_string())
        .text("response_format", "json");

    // Language hint only valid for transcriptions, not translations
    if let Some(lang) = request.language {
        if !request.translate {
            form = form.text("language", lang.to_string());
        }
    }

    Ok(form)
}

/// Parse the Whisper API response.
async fn parse_whisper_response(
    response: reqwest::Response,
    translated: bool,
) -> Result<TranscriptionResponse> {
    let status = response.status();
    let body: serde_json::Value = response
        .json()
        .await
        .map_err(|e| LlmError::ProviderRequest(format!("failed to parse response JSON: {}", e)))?;

    if !status.is_success() {
        let message = body["error"]["message"].as_str().unwrap_or("unknown error");
        return Err(LlmError::ProviderRequest(format!(
            "Whisper API error ({}): {}",
            status, message
        ))
        .into());
    }

    let text = body["text"]
        .as_str()
        .ok_or_else(|| {
            LlmError::ProviderRequest("missing text in transcription response".to_string())
        })?
        .to_string();

    let duration_secs = body["duration"].as_f64();

    Ok(TranscriptionResponse {
        text,
        duration_secs,
        translated,
    })
}

/// Check if a provider supports Whisper-compatible transcription.
///
/// Supports: OpenAI, Groq, Gemini (via OpenAI-compatible endpoint).
pub fn supports_whisper_transcription(provider: &ProviderConfig) -> bool {
    let base = provider.base_url.to_lowercase();
    base.contains("openai.com")
        || base.contains("groq.com")
        || base.contains("googleapis.com")
        || base.contains("generativelanguage.googleapis.com")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_transcription_request<'a>(
        audio: &'a [u8],
        language: Option<&'a str>,
        translate: bool,
    ) -> TranscriptionRequest<'a> {
        TranscriptionRequest {
            audio_bytes: audio,
            filename: "test.mp3",
            mime_type: "audio/mpeg",
            model: "whisper-1",
            language,
            translate,
        }
    }

    #[test]
    fn test_build_whisper_endpoint_openai_transcription() {
        let url = build_whisper_endpoint("https://api.openai.com", false);
        assert_eq!(url, "https://api.openai.com/v1/audio/transcriptions");
    }

    #[test]
    fn test_build_whisper_endpoint_openai_translation() {
        let url = build_whisper_endpoint("https://api.openai.com", true);
        assert_eq!(url, "https://api.openai.com/v1/audio/translations");
    }

    #[test]
    fn test_build_whisper_endpoint_groq_transcription() {
        let url = build_whisper_endpoint("https://api.groq.com/openai", false);
        assert_eq!(
            url,
            "https://api.groq.com/openai/openai/v1/audio/transcriptions"
        );
    }

    #[test]
    fn test_build_whisper_endpoint_groq_translation() {
        let url = build_whisper_endpoint("https://api.groq.com/openai", true);
        assert_eq!(
            url,
            "https://api.groq.com/openai/openai/v1/audio/translations"
        );
    }

    #[test]
    fn test_build_whisper_endpoint_gemini() {
        let url = build_whisper_endpoint(
            "https://generativelanguage.googleapis.com/v1beta/openai",
            false,
        );
        assert_eq!(
            url,
            "https://generativelanguage.googleapis.com/v1beta/openai/v1/audio/transcriptions"
        );
    }

    #[test]
    fn test_build_whisper_endpoint_gemini_translation() {
        let url = build_whisper_endpoint(
            "https://generativelanguage.googleapis.com/v1beta/openai",
            true,
        );
        assert_eq!(
            url,
            "https://generativelanguage.googleapis.com/v1beta/openai/v1/audio/translations"
        );
    }

    #[test]
    fn test_build_whisper_endpoint_trailing_slash() {
        let url = build_whisper_endpoint("https://api.openai.com/", false);
        assert_eq!(url, "https://api.openai.com/v1/audio/transcriptions");
    }

    #[test]
    fn test_build_multipart_form_with_language() {
        let audio = vec![0u8; 100];
        let request = test_transcription_request(&audio, Some("en"), false);
        // Form builds successfully with language hint for transcription
        assert!(build_multipart_form(&request).is_ok());
    }

    #[test]
    fn test_build_multipart_form_translation_ignores_language() {
        let audio = vec![0u8; 100];
        // Translation mode with language hint — should still build OK
        // (language is silently ignored for translations)
        let request = test_transcription_request(&audio, Some("es"), true);
        assert!(build_multipart_form(&request).is_ok());
    }

    #[test]
    fn test_build_multipart_form_no_language() {
        let audio = vec![0u8; 100];
        let request = test_transcription_request(&audio, None, false);
        assert!(build_multipart_form(&request).is_ok());
    }

    #[test]
    fn test_build_multipart_form_success() {
        let audio = vec![0u8; 100];
        let request = test_transcription_request(&audio, None, false);
        // Verify the form builds without error — reqwest::multipart::Form
        // is opaque so we can't inspect individual fields.
        assert!(build_multipart_form(&request).is_ok());
    }

    #[test]
    fn test_supports_whisper_transcription_openai() {
        let provider = ProviderConfig {
            api_type: crate::config::ApiType::OpenAiCompletions,
            base_url: "https://api.openai.com".to_string(),
            api_key: "test-key".to_string(),
            name: Some("OpenAI".to_string()),
            use_bearer_auth: false,
            extra_headers: vec![],
        };
        assert!(supports_whisper_transcription(&provider));
    }

    #[test]
    fn test_supports_whisper_transcription_groq() {
        let provider = ProviderConfig {
            api_type: crate::config::ApiType::OpenAiCompletions,
            base_url: "https://api.groq.com/openai".to_string(),
            api_key: "test-key".to_string(),
            name: Some("Groq".to_string()),
            use_bearer_auth: false,
            extra_headers: vec![],
        };
        assert!(supports_whisper_transcription(&provider));
    }

    #[test]
    fn test_supports_whisper_transcription_gemini() {
        let provider = ProviderConfig {
            api_type: crate::config::ApiType::Gemini,
            base_url: "https://generativelanguage.googleapis.com/v1beta/openai".to_string(),
            api_key: "test-key".to_string(),
            name: Some("Gemini".to_string()),
            use_bearer_auth: false,
            extra_headers: vec![],
        };
        assert!(supports_whisper_transcription(&provider));
    }

    #[test]
    fn test_supports_whisper_transcription_anthropic_not_supported() {
        let provider = ProviderConfig {
            api_type: crate::config::ApiType::Anthropic,
            base_url: "https://api.anthropic.com".to_string(),
            api_key: "test-key".to_string(),
            name: Some("Anthropic".to_string()),
            use_bearer_auth: false,
            extra_headers: vec![],
        };
        assert!(!supports_whisper_transcription(&provider));
    }

    #[test]
    fn test_supports_whisper_transcription_openrouter_not_supported() {
        let provider = ProviderConfig {
            api_type: crate::config::ApiType::OpenAiCompletions,
            base_url: "https://openrouter.ai/api/v1".to_string(),
            api_key: "test-key".to_string(),
            name: Some("OpenRouter".to_string()),
            use_bearer_auth: false,
            extra_headers: vec![],
        };
        assert!(!supports_whisper_transcription(&provider));
    }

    #[test]
    fn test_supports_whisper_transcription_case_insensitive() {
        let provider = ProviderConfig {
            api_type: crate::config::ApiType::OpenAiCompletions,
            base_url: "https://API.OPENAI.COM".to_string(),
            api_key: "test-key".to_string(),
            name: Some("OpenAI".to_string()),
            use_bearer_auth: false,
            extra_headers: vec![],
        };
        assert!(supports_whisper_transcription(&provider));
    }

    #[test]
    fn test_transcription_request_lifetimes() {
        let audio = vec![1u8, 2, 3, 4, 5];
        let filename = "recording.mp3";
        let mime = "audio/mpeg";
        let model = "whisper-large-v3";
        let lang = "es";

        let request = TranscriptionRequest {
            audio_bytes: &audio,
            filename,
            mime_type: mime,
            model,
            language: Some(lang),
            translate: false,
        };

        assert_eq!(request.audio_bytes.len(), 5);
        assert_eq!(request.filename, "recording.mp3");
        assert_eq!(request.mime_type, "audio/mpeg");
        assert_eq!(request.model, "whisper-large-v3");
        assert_eq!(request.language, Some("es"));
        assert!(!request.translate);
    }

    #[test]
    fn test_transcription_response_fields() {
        let response = TranscriptionResponse {
            text: "Hello, world!".to_string(),
            duration_secs: Some(5.5),
            translated: false,
        };

        assert_eq!(response.text, "Hello, world!");
        assert_eq!(response.duration_secs, Some(5.5));
        assert!(!response.translated);
    }

    #[test]
    fn test_transcription_response_translation_mode() {
        let response = TranscriptionResponse {
            text: "Hello, world!".to_string(),
            duration_secs: None,
            translated: true,
        };

        assert!(response.translated);
        assert!(response.duration_secs.is_none());
    }

    #[test]
    fn test_build_multipart_form_invalid_mime() {
        let audio = vec![0u8; 100];
        let request = TranscriptionRequest {
            audio_bytes: &audio,
            filename: "test.mp3",
            mime_type: "invalid/mime type with spaces",
            model: "whisper-1",
            language: None,
            translate: false,
        };
        let result = build_multipart_form(&request);
        assert!(result.is_err());
    }
}
