//! Attachment download and processing for channel messages.
//!
//! Handles image, text, and audio attachments — downloading from URLs,
//! base64 encoding for vision models, inlining text content, and
//! transcribing audio via the configured voice model.
//!
//! When `save_attachments` is enabled on a channel, downloaded files are
//! persisted to `workspace/saved/` and tracked in the `saved_attachments`
//! table for later recall.

use crate::AgentDeps;
use crate::config::ApiType;
use rig::message::{ImageMediaType, MimeType, UserContent};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

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

/// A saved attachment paired with its raw bytes, used to avoid re-downloading.
pub(crate) type SavedAttachmentWithBytes = (SavedAttachmentMeta, Vec<u8>);

/// Build LLM-ready `UserContent` from pre-downloaded bytes. Used when
/// `save_attachments` is enabled so we don't re-download from the URL.
///
/// Audio attachments are NOT re-transcribed here — audio requires an LLM call
/// which needs `AgentDeps`. When saving is enabled, audio files are saved to
/// disk and transcribed via the normal `download_attachments` path (which will
/// be called separately).
pub(crate) fn content_from_bytes(bytes: &[u8], attachment: &crate::Attachment) -> UserContent {
    let is_image = IMAGE_MIME_PREFIXES
        .iter()
        .any(|p| attachment.mime_type.starts_with(p));
    let is_text = TEXT_MIME_PREFIXES
        .iter()
        .any(|p| attachment.mime_type.starts_with(p));

    if is_image {
        use base64::Engine as _;
        let base64_data = base64::engine::general_purpose::STANDARD.encode(bytes);
        let media_type = ImageMediaType::from_mime_type(&attachment.mime_type);
        UserContent::image_base64(base64_data, media_type, None)
    } else if is_text {
        let content = String::from_utf8_lossy(bytes).into_owned();
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
        UserContent::text(format!(
            "<file name=\"{}\" mime=\"{}\">\n{}\n</file>",
            attachment.filename, attachment.mime_type, truncated
        ))
    } else {
        let size_str = attachment
            .size_bytes
            .map(|s| format!("{:.1} KB", s as f64 / 1024.0))
            .unwrap_or_else(|| format!("{:.1} KB", bytes.len() as f64 / 1024.0));
        UserContent::text(format!(
            "[Attachment: {} ({}, {})]",
            attachment.filename, attachment.mime_type, size_str
        ))
    }
}

// ---------------------------------------------------------------------------
// Attachment persistence
// ---------------------------------------------------------------------------

/// Metadata for a saved attachment, returned after persisting to disk and DB.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct SavedAttachmentMeta {
    pub id: String,
    pub filename: String,
    pub saved_filename: String,
    pub mime_type: String,
    pub size_bytes: u64,
}

/// Download and save channel attachments to `workspace/saved/`, recording each
/// in the `saved_attachments` table. Returns metadata for each saved file so
/// the caller can annotate the conversation message.
///
/// Also returns the raw bytes keyed by index so the caller can reuse them for
/// LLM processing without a second download.
pub(crate) async fn save_channel_attachments(
    pool: &sqlx::SqlitePool,
    http: &reqwest::Client,
    channel_id: &str,
    saved_dir: &Path,
    attachments: &[crate::Attachment],
) -> Vec<(SavedAttachmentMeta, Vec<u8>)> {
    let mut results = Vec::with_capacity(attachments.len());

    for attachment in attachments {
        // Pre-saved: file is already on disk from a portal upload — look up the
        // DB record by id + channel_id and re-derive the path from saved_dir
        // instead of trusting struct fields.
        if let Some(ref pre_saved_id) = attachment.pre_saved_id {
            match lookup_pre_saved(pool, pre_saved_id, channel_id, saved_dir).await {
                Ok(Some(pair)) => results.push(pair),
                Ok(None) => {
                    tracing::warn!(
                        attachment_id = %pre_saved_id,
                        channel_id,
                        "pre-saved attachment not found or wrong channel"
                    );
                }
                Err(error) => {
                    tracing::warn!(
                        %error,
                        attachment_id = %pre_saved_id,
                        "failed to load pre-saved attachment"
                    );
                }
            }
            continue;
        }

        let safe_name = match sanitize_filename(&attachment.filename) {
            Ok(name) => name,
            Err(error) => {
                tracing::warn!(
                    %error,
                    filename = %attachment.filename,
                    "rejected unsafe attachment filename"
                );
                continue;
            }
        };

        let bytes = match download_attachment_bytes(http, attachment).await {
            Ok(bytes) => bytes,
            Err(error) => {
                tracing::warn!(
                    %error,
                    filename = %attachment.filename,
                    "failed to download attachment for saving"
                );
                continue;
            }
        };

        let saved_filename = match deduplicate_filename(pool, saved_dir, &safe_name).await {
            Ok(name) => name,
            Err(error) => {
                tracing::warn!(
                    %error,
                    filename = %attachment.filename,
                    "failed to compute unique filename"
                );
                continue;
            }
        };

        let disk_path = saved_dir.join(&saved_filename);

        // Use create_new for atomic creation — prevents race conditions where
        // two concurrent saves compute the same deduplicated name.
        match write_file_atomic(&disk_path, &bytes).await {
            Ok(()) => {}
            Err(error) => {
                tracing::warn!(
                    %error,
                    path = %disk_path.display(),
                    "failed to write attachment to disk"
                );
                continue;
            }
        }

        let id = uuid::Uuid::new_v4().to_string();
        let size_bytes = bytes.len() as u64;
        let disk_path_str = disk_path.to_string_lossy().to_string();

        let insert_result = sqlx::query(
            "INSERT INTO saved_attachments \
             (id, channel_id, original_filename, saved_filename, mime_type, size_bytes, disk_path) \
             VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&id)
        .bind(channel_id)
        .bind(&attachment.filename)
        .bind(&saved_filename)
        .bind(&attachment.mime_type)
        .bind(size_bytes as i64)
        .bind(&disk_path_str)
        .execute(pool)
        .await;

        if let Err(error) = insert_result {
            tracing::warn!(
                %error,
                filename = %attachment.filename,
                "failed to record saved attachment in database"
            );
            // File is on disk but not tracked — clean up
            let _ = tokio::fs::remove_file(&disk_path).await;
            continue;
        }

        tracing::info!(
            attachment_id = %id,
            original = %attachment.filename,
            saved = %saved_filename,
            size = size_bytes,
            "saved channel attachment"
        );

        results.push((
            SavedAttachmentMeta {
                id,
                filename: attachment.filename.clone(),
                saved_filename,
                mime_type: attachment.mime_type.clone(),
                size_bytes,
            },
            bytes,
        ));
    }

    results
}

/// Build a text annotation summarising saved attachments for inclusion in
/// conversation history. This lets the LLM see file references on later turns.
pub(crate) fn format_attachment_annotation(saved: &[SavedAttachmentMeta]) -> String {
    if saved.is_empty() {
        return String::new();
    }

    let items: Vec<String> = saved
        .iter()
        .map(|attachment| {
            let size_str = if attachment.size_bytes >= 1024 * 1024 {
                format!("{:.1} MB", attachment.size_bytes as f64 / (1024.0 * 1024.0))
            } else {
                format!("{:.0} KB", attachment.size_bytes as f64 / 1024.0)
            };
            format!(
                "{} ({}, {}, id:{})",
                attachment.filename,
                attachment.mime_type,
                size_str,
                attachment.id.get(..8).unwrap_or(&attachment.id)
            )
        })
        .collect();

    if items.len() == 1 {
        format!("[Attachment saved: {}]", items[0])
    } else {
        format!("[{} attachments saved: {}]", items.len(), items.join(", "))
    }
}

/// Reconstruct attachment annotation from message metadata JSON.
///
/// Called when loading conversation history so older messages that had
/// attachments still show the `[Attachments: ...]` annotation.
pub(crate) fn annotation_from_metadata(metadata: &serde_json::Value) -> Option<String> {
    let attachments = metadata.get("attachments")?.as_array()?;
    if attachments.is_empty() {
        return None;
    }

    let saved: Vec<SavedAttachmentMeta> = attachments
        .iter()
        .filter_map(|value| serde_json::from_value(value.clone()).ok())
        .collect();

    if saved.is_empty() {
        return None;
    }

    Some(format_attachment_annotation(&saved))
}

/// Sanitize a user-provided filename to prevent path traversal attacks.
///
/// Extracts only the file name component (strips directory separators and
/// parent references), rejects `.`, `..`, and empty results. Falls back to
/// `attachment` if the stem is entirely stripped.
fn sanitize_filename(raw: &str) -> Result<String, String> {
    // Extract just the filename component — strips any directory prefixes
    let basename = Path::new(raw)
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_default();

    // Reject dangerous or empty names
    let trimmed = basename.trim();
    if trimmed.is_empty() || trimmed == "." || trimmed == ".." {
        // If the original had an extension, preserve it with a safe stem
        let extension = Path::new(raw)
            .extension()
            .map(|e| e.to_string_lossy().to_string());
        return match extension {
            Some(ext) => Ok(format!("attachment.{ext}")),
            None => Ok("attachment".to_string()),
        };
    }

    Ok(trimmed.to_string())
}

/// Compute a unique filename within `saved_dir`, appending `_N` suffixes
/// to avoid collisions with existing files on disk or in the database.
async fn deduplicate_filename(
    pool: &sqlx::SqlitePool,
    saved_dir: &Path,
    original: &str,
) -> Result<String, String> {
    // Check if the original name is available (no DB record AND no file on disk)
    if !filename_taken(pool, saved_dir, original).await {
        return Ok(original.to_string());
    }

    // Split into stem + extension
    let path = PathBuf::from(original);
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| original.to_string());
    let extension = path
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()));

    for counter in 2..=999 {
        let candidate = match &extension {
            Some(ext) => format!("{stem}_{counter}{ext}"),
            None => format!("{stem}_{counter}"),
        };
        if !filename_taken(pool, saved_dir, &candidate).await {
            return Ok(candidate);
        }
    }

    Err(format!(
        "could not find unique filename for '{original}' after 998 attempts"
    ))
}

/// Write bytes to a file atomically using `create_new` to prevent races.
///
/// If the file already exists (`AlreadyExists` error), returns an error
/// rather than silently overwriting.
async fn write_file_atomic(path: &Path, bytes: &[u8]) -> std::result::Result<(), String> {
    use tokio::io::AsyncWriteExt;

    let file = tokio::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .await
        .map_err(|error| format!("failed to create file {}: {error}", path.display()))?;

    let mut writer = tokio::io::BufWriter::new(file);
    writer
        .write_all(bytes)
        .await
        .map_err(|error| format!("failed to write to {}: {error}", path.display()))?;
    writer
        .flush()
        .await
        .map_err(|error| format!("failed to flush {}: {error}", path.display()))?;

    Ok(())
}

/// Check whether a filename is already used — either by a DB record or a file
/// on disk. Both must be clear for the name to be available.
async fn filename_taken(pool: &sqlx::SqlitePool, saved_dir: &Path, filename: &str) -> bool {
    // Check database first (fast)
    let db_exists = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM saved_attachments WHERE saved_filename = ?",
    )
    .bind(filename)
    .fetch_one(pool)
    .await
    .unwrap_or(1)
        > 0;

    if db_exists {
        return true;
    }

    // Also check filesystem in case of orphaned files
    saved_dir.join(filename).exists()
}

// ---------------------------------------------------------------------------
// Pre-saved attachment lookup
// ---------------------------------------------------------------------------

/// Look up a pre-saved attachment by id + channel_id and read its bytes from
/// disk. Returns `None` if the row doesn't exist or the channel doesn't match.
/// The path is re-derived from `saved_dir` + the `saved_filename` column to
/// avoid trusting the `disk_path` column.
async fn lookup_pre_saved(
    pool: &sqlx::SqlitePool,
    attachment_id: &str,
    channel_id: &str,
    saved_dir: &Path,
) -> anyhow::Result<Option<(SavedAttachmentMeta, Vec<u8>)>> {
    use sqlx::Row as _;

    let row = sqlx::query(
        "SELECT id, original_filename, saved_filename, mime_type, size_bytes \
         FROM saved_attachments WHERE id = ? AND channel_id = ?",
    )
    .bind(attachment_id)
    .bind(channel_id)
    .fetch_optional(pool)
    .await?;

    let Some(row) = row else {
        return Ok(None);
    };

    let id: String = row.try_get("id")?;
    let original_filename: String = row.try_get("original_filename")?;
    let saved_filename: String = row.try_get("saved_filename")?;
    let mime_type: String = row.try_get("mime_type")?;
    let size_bytes: i64 = row.try_get("size_bytes")?;

    let path = saved_dir.join(&saved_filename);
    if !path.starts_with(saved_dir) {
        anyhow::bail!("pre-saved attachment path escapes saved dir: {saved_filename}");
    }

    let bytes = tokio::fs::read(&path).await?;

    Ok(Some((
        SavedAttachmentMeta {
            id,
            filename: original_filename,
            saved_filename,
            mime_type,
            size_bytes: u64::try_from(size_bytes).unwrap_or(0),
        },
        bytes,
    )))
}

// ---------------------------------------------------------------------------
// Portal upload — persist bytes directly (no URL download)
// ---------------------------------------------------------------------------

/// Save raw bytes from a portal upload to `workspace/saved/` and insert a
/// record into `saved_attachments`. Used by the portal attachment upload
/// endpoint where the file is already in memory (no URL to download from).
pub async fn persist_attachment_bytes(
    pool: &sqlx::SqlitePool,
    channel_id: &str,
    saved_dir: &Path,
    original_filename: &str,
    mime_type: &str,
    bytes: &[u8],
) -> anyhow::Result<SavedAttachmentMeta> {
    let safe_name = sanitize_filename(original_filename)
        .map_err(|e| anyhow::anyhow!("unsafe filename: {e}"))?;

    tokio::fs::create_dir_all(saved_dir).await?;

    let saved_filename = deduplicate_filename(pool, saved_dir, &safe_name)
        .await
        .map_err(|e| anyhow::anyhow!("filename dedup failed: {e}"))?;
    let disk_path = saved_dir.join(&saved_filename);

    write_file_atomic(&disk_path, bytes)
        .await
        .map_err(|e| anyhow::anyhow!("write failed: {e}"))?;

    let id = uuid::Uuid::new_v4().to_string();
    let size_bytes = bytes.len() as u64;
    let disk_path_str = disk_path.to_string_lossy().to_string();

    sqlx::query(
        "INSERT INTO saved_attachments \
         (id, channel_id, original_filename, saved_filename, mime_type, size_bytes, disk_path) \
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(channel_id)
    .bind(original_filename)
    .bind(&saved_filename)
    .bind(mime_type)
    .bind(size_bytes as i64)
    .bind(&disk_path_str)
    .execute(pool)
    .await?;

    tracing::info!(
        attachment_id = %id,
        original = %original_filename,
        saved = %saved_filename,
        size = size_bytes,
        channel_id,
        "persisted portal upload attachment"
    );

    Ok(SavedAttachmentMeta {
        id,
        filename: original_filename.to_string(),
        saved_filename,
        mime_type: mime_type.to_string(),
        size_bytes,
    })
}
