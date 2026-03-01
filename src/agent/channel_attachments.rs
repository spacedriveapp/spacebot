//! Attachment download and processing for channel messages.
//!
//! Handles image, text, and audio attachments — downloading from URLs,
//! base64 encoding for vision models, inlining text content, and
//! transcribing audio via the configured voice model.

use crate::AgentDeps;
use crate::config::ApiType;
use rig::message::{ImageMediaType, MimeType, UserContent};

/// Image MIME types we support for vision.
const IMAGE_MIME_PREFIXES: &[&str] = &["image/jpeg", "image/png", "image/gif", "image/webp"];

/// Text-based MIME types where we inline the content.
const TEXT_MIME_PREFIXES: &[&str] = &[
    "text/",
    "application/json",
    "application/xml",
    "application/javascript",
    "application/typescript",
    "application/toml",
    "application/yaml",
];

/// Download attachments and convert them to LLM-ready UserContent parts.
///
/// Images become `UserContent::Image` (base64). Text files get inlined.
/// Other file types get a metadata-only description.
pub(crate) async fn download_attachments(
    deps: &AgentDeps,
    attachments: &[crate::Attachment],
) -> Vec<UserContent> {
    let http = deps.llm_manager.http_client();
    let mut parts = Vec::new();

    for attachment in attachments {
        let is_image = IMAGE_MIME_PREFIXES
            .iter()
            .any(|p| attachment.mime_type.starts_with(p));
        let is_text = TEXT_MIME_PREFIXES
            .iter()
            .any(|p| attachment.mime_type.starts_with(p));

        let content = if is_image {
            download_image_attachment(http, attachment).await
        } else if is_text {
            download_text_attachment(http, attachment).await
        } else if attachment.mime_type.starts_with("audio/") {
            transcribe_audio_attachment(deps, http, attachment).await
        } else {
            let size_str = attachment
                .size_bytes
                .map(|s| format!("{:.1} KB", s as f64 / 1024.0))
                .unwrap_or_else(|| "unknown size".into());
            UserContent::text(format!(
                "[Attachment: {} ({}, {})]",
                attachment.filename, attachment.mime_type, size_str
            ))
        };

        parts.push(content);
    }

    parts
}

/// Download raw bytes from an attachment URL, including auth if present.
///
/// When `auth_header` is set (Slack), uses a no-redirect client and manually
/// follows redirects so the `Authorization` header isn't silently stripped on
/// cross-origin redirects. For public URLs (Discord/Telegram), uses a plain GET.
async fn download_attachment_bytes(
    http: &reqwest::Client,
    attachment: &crate::Attachment,
) -> std::result::Result<Vec<u8>, String> {
    if attachment.auth_header.is_some() {
        download_attachment_bytes_with_auth(attachment).await
    } else {
        let response = http
            .get(&attachment.url)
            .send()
            .await
            .map_err(|e| e.to_string())?;
        if !response.status().is_success() {
            return Err(format!("HTTP {}", response.status()));
        }
        response
            .bytes()
            .await
            .map(|b| b.to_vec())
            .map_err(|e| e.to_string())
    }
}

/// Slack-specific download: manually follows redirects, only forwarding the
/// Authorization header when the redirect target shares the same host as the
/// original URL. This prevents credential leakage on cross-origin redirects.
async fn download_attachment_bytes_with_auth(
    attachment: &crate::Attachment,
) -> std::result::Result<Vec<u8>, String> {
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs(60))
        .build()
        .map_err(|e| format!("failed to build HTTP client: {e}"))?;

    let auth = attachment.auth_header.as_deref().unwrap_or_default();
    let original_url =
        reqwest::Url::parse(&attachment.url).map_err(|e| format!("invalid attachment URL: {e}"))?;
    let original_host = original_url.host_str().unwrap_or_default().to_owned();
    let mut current_url = original_url;

    for hop in 0..5 {
        let same_host = current_url.host_str().unwrap_or_default() == original_host;

        let mut request = client.get(current_url.clone());
        if same_host {
            request = request.header(reqwest::header::AUTHORIZATION, auth);
        }

        tracing::debug!(hop, url = %current_url, same_host, "following attachment redirect");

        let response = request.send().await.map_err(|e| e.to_string())?;
        let status = response.status();

        if status.is_redirection() {
            let location = response
                .headers()
                .get(reqwest::header::LOCATION)
                .ok_or_else(|| format!("redirect without Location header ({status})"))?;
            let location_str = location
                .to_str()
                .map_err(|e| format!("invalid Location header: {e}"))?;
            current_url = current_url
                .join(location_str)
                .map_err(|e| format!("invalid redirect URL: {e}"))?;
            continue;
        }

        if !status.is_success() {
            return Err(format!("HTTP {}", status));
        }

        return response
            .bytes()
            .await
            .map(|b| b.to_vec())
            .map_err(|e| e.to_string());
    }

    Err("too many redirects".into())
}

/// Download an image attachment and encode it as base64 for the LLM.
async fn download_image_attachment(
    http: &reqwest::Client,
    attachment: &crate::Attachment,
) -> UserContent {
    let bytes = match download_attachment_bytes(http, attachment).await {
        Ok(b) => b,
        Err(error) => {
            tracing::warn!(%error, filename = %attachment.filename, "failed to download image");
            return UserContent::text(format!(
                "[Failed to download image: {}]",
                attachment.filename
            ));
        }
    };

    use base64::Engine as _;
    let base64_data = base64::engine::general_purpose::STANDARD.encode(&bytes);
    let media_type = ImageMediaType::from_mime_type(&attachment.mime_type);

    tracing::info!(
        filename = %attachment.filename,
        mime = %attachment.mime_type,
        size = bytes.len(),
        "downloaded image attachment"
    );

    UserContent::image_base64(base64_data, media_type, None)
}

/// Download an audio attachment and transcribe it with the configured voice model.
async fn transcribe_audio_attachment(
    deps: &AgentDeps,
    http: &reqwest::Client,
    attachment: &crate::Attachment,
) -> UserContent {
    let bytes = match download_attachment_bytes(http, attachment).await {
        Ok(b) => b,
        Err(error) => {
            tracing::warn!(%error, filename = %attachment.filename, "failed to download audio");
            return UserContent::text(format!(
                "[Failed to download audio: {}]",
                attachment.filename
            ));
        }
    };

    tracing::info!(
        filename = %attachment.filename,
        mime = %attachment.mime_type,
        size = bytes.len(),
        "downloaded audio attachment"
    );

    let routing = deps.runtime_config.routing.load();
    let voice_model = routing.voice.trim();
    if voice_model.is_empty() {
        return UserContent::text(format!(
            "[Audio attachment received but no voice model is configured in routing.voice: {}]",
            attachment.filename
        ));
    }

    let (provider_id, model_name) = match deps.llm_manager.resolve_model(voice_model) {
        Ok(parts) => parts,
        Err(error) => {
            tracing::warn!(%error, model = %voice_model, "invalid voice model route");
            return UserContent::text(format!(
                "[Audio transcription failed for {}: invalid voice model '{}']",
                attachment.filename, voice_model
            ));
        }
    };

    let provider = match deps.llm_manager.get_provider(&provider_id) {
        Ok(provider) => provider,
        Err(error) => {
            tracing::warn!(%error, provider = %provider_id, "voice provider not configured");
            return UserContent::text(format!(
                "[Audio transcription failed for {}: provider '{}' is not configured]",
                attachment.filename, provider_id
            ));
        }
    };

    if provider.api_type == ApiType::Anthropic {
        return UserContent::text(format!(
            "[Audio transcription failed for {}: provider '{}' does not support input_audio on this endpoint]",
            attachment.filename, provider_id
        ));
    }

    let format = audio_format_for_attachment(attachment);
    use base64::Engine as _;
    let base64_audio = base64::engine::general_purpose::STANDARD.encode(&bytes);

    let endpoint = format!(
        "{}/v1/chat/completions",
        provider.base_url.trim_end_matches('/')
    );
    let body = serde_json::json!({
        "model": model_name,
        "messages": [{
            "role": "user",
            "content": [
                {
                    "type": "text",
                    "text": "Transcribe this audio verbatim. Return only the transcription text."
                },
                {
                    "type": "input_audio",
                    "input_audio": {
                        "data": base64_audio,
                        "format": format,
                    }
                }
            ]
        }],
        "temperature": 0
    });

    let response = match deps
        .llm_manager
        .http_client()
        .post(&endpoint)
        .header("authorization", format!("Bearer {}", provider.api_key))
        .header("content-type", "application/json")
        .json(&body)
        .send()
        .await
    {
        Ok(response) => response,
        Err(error) => {
            tracing::warn!(%error, model = %voice_model, "voice transcription request failed");
            return UserContent::text(format!(
                "[Audio transcription failed for {}]",
                attachment.filename
            ));
        }
    };

    let status = response.status();
    let response_body = match response.json::<serde_json::Value>().await {
        Ok(body) => body,
        Err(error) => {
            tracing::warn!(%error, model = %voice_model, "invalid transcription response");
            return UserContent::text(format!(
                "[Audio transcription failed for {}]",
                attachment.filename
            ));
        }
    };

    if !status.is_success() {
        let message = response_body["error"]["message"]
            .as_str()
            .unwrap_or("unknown error");
        tracing::warn!(
            status = %status,
            model = %voice_model,
            error = %message,
            "voice transcription provider returned error"
        );
        return UserContent::text(format!(
            "[Audio transcription failed for {}: {}]",
            attachment.filename, message
        ));
    }

    let transcript = extract_transcript_text(&response_body);
    if transcript.is_empty() {
        tracing::warn!(model = %voice_model, "empty transcription returned");
        return UserContent::text(format!(
            "[Audio transcription returned empty text for {}]",
            attachment.filename
        ));
    }

    UserContent::text(format!(
        "<voice_transcript name=\"{}\" mime=\"{}\">\n{}\n</voice_transcript>",
        attachment.filename, attachment.mime_type, transcript
    ))
}

fn audio_format_for_attachment(attachment: &crate::Attachment) -> &'static str {
    let mime = attachment.mime_type.to_lowercase();
    if mime.contains("mpeg") || mime.contains("mp3") {
        return "mp3";
    }
    if mime.contains("wav") {
        return "wav";
    }
    if mime.contains("flac") {
        return "flac";
    }
    if mime.contains("aac") {
        return "aac";
    }
    if mime.contains("ogg") {
        return "ogg";
    }
    if mime.contains("mp4") || mime.contains("m4a") {
        return "m4a";
    }

    match attachment
        .filename
        .rsplit('.')
        .next()
        .unwrap_or_default()
        .to_lowercase()
        .as_str()
    {
        "mp3" => "mp3",
        "wav" => "wav",
        "flac" => "flac",
        "aac" => "aac",
        "m4a" | "mp4" => "m4a",
        "oga" | "ogg" => "ogg",
        _ => "ogg",
    }
}

fn extract_transcript_text(body: &serde_json::Value) -> String {
    if let Some(text) = body["choices"][0]["message"]["content"].as_str() {
        return text.trim().to_string();
    }

    let Some(parts) = body["choices"][0]["message"]["content"].as_array() else {
        return String::new();
    };

    parts
        .iter()
        .filter_map(|part| {
            if part["type"].as_str() == Some("text") {
                part["text"].as_str().map(str::trim)
            } else {
                None
            }
        })
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Download a text attachment and inline its content for the LLM.
async fn download_text_attachment(
    http: &reqwest::Client,
    attachment: &crate::Attachment,
) -> UserContent {
    let bytes = match download_attachment_bytes(http, attachment).await {
        Ok(b) => b,
        Err(error) => {
            tracing::warn!(%error, filename = %attachment.filename, "failed to download text file");
            return UserContent::text(format!(
                "[Failed to download file: {}]",
                attachment.filename
            ));
        }
    };

    let content = String::from_utf8_lossy(&bytes).into_owned();

    // Truncate very large files to avoid blowing up context
    let truncated = if content.len() > 50_000 {
        let end = content.floor_char_boundary(50_000);
        format!(
            "{}...\n[truncated — {} bytes total]",
            &content[..end],
            content.len()
        )
    } else {
        content
    };

    tracing::info!(
        filename = %attachment.filename,
        mime = %attachment.mime_type,
        "downloaded text attachment"
    );

    UserContent::text(format!(
        "<file name=\"{}\" mime=\"{}\">\n{}\n</file>",
        attachment.filename, attachment.mime_type, truncated
    ))
}
