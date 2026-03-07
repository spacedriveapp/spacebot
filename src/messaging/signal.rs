//! Signal messaging adapter using signal-cli JSON-RPC daemon.
//!
//! Connects to a running `signal-cli daemon --http` instance for sending and
//! receiving Signal messages via its JSON-RPC and Server-Sent Events (SSE) APIs.
//!
//! ## Architecture
//!
//! - **Inbound:** SSE stream from `{http_url}/api/v1/events?account={account}` with
//!   automatic reconnection and exponential backoff (2s → 60s).
//! - **Outbound:** JSON-RPC `send` calls to `{http_url}/api/v1/rpc`. DM recipients
//!   must be passed as a JSON **array** (signal-cli requirement). Group messages use
//!   `groupId` instead of `recipient`.
//! - **Typing:** JSON-RPC `sendTyping` method with repeating task (Signal indicators
//!   expire after ~5s).
//! - **Attachments outbound:** Written to `{tmp_dir}/` as temp files, file paths passed
//!   in the `attachments` JSON array, cleaned up after send.
//! - **Attachments inbound:** signal-cli provides file paths on disk; currently treated
//!   as opaque (message text falls back to `[Attachment]` for attachment-only messages).
//! - **Streaming:** Not supported (Signal can't edit sent messages). `StreamStart`,
//!   `StreamChunk`, and `StreamEnd` are no-ops.
//!
//! ## Limitations
//!
//! - **Threading:** Signal has no native thread concept. `ThreadReply` responses are
//!   sent as regular messages without thread context.
//! - **Rich formatting:** Signal has no rich formatting (bold, italic, etc.).
//!   `RichMessage` is sent as plain text.
//! - **Reactions:** Signal reactions are supported via JSON-RPC but require complex
//!   target identification (author UUID + timestamp). Not currently implemented.
//! - **Ephemeral messages:** Not supported; sent as regular messages.
//! - **Scheduled messages:** Not supported; sent immediately.

use crate::config::SignalPermissions;
use crate::messaging::traits::{
    InboundStream, Messaging, apply_runtime_adapter_to_conversation_id,
};
use crate::{InboundMessage, MessageContent, OutboundResponse, StatusUpdate, metadata_keys};

use anyhow::Context as _;
use arc_swap::ArcSwap;
use futures::StreamExt;
use lru::LruCache;
use serde::Deserialize;
use std::collections::HashMap;
use std::num::NonZeroUsize;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{RwLock, mpsc};
use tokio::task::JoinHandle;
use uuid::Uuid;

// ── constants ───────────────────────────────────────────────────

/// Maximum SSE line buffer size before reset (prevents OOM from runaway streams).
const MAX_SSE_BUFFER_SIZE: usize = 1024 * 1024; // 1 MiB

/// Maximum single SSE event size (prevents huge JSON payloads).
const MAX_SSE_EVENT_SIZE: usize = 256 * 1024; // 256 KiB

/// Maximum JSON-RPC response body size.
const MAX_RPC_RESPONSE_SIZE: usize = 1024 * 1024; // 1 MiB

/// Prefix used to encode group targets in the reply target string.
const GROUP_TARGET_PREFIX: &str = "group:";

/// Signal-cli health check endpoint.
const HEALTH_ENDPOINT: &str = "/api/v1/check";

/// JSON-RPC endpoint path.
const RPC_ENDPOINT: &str = "/api/v1/rpc";

/// SSE events endpoint path.
const SSE_ENDPOINT: &str = "/api/v1/events";

/// Typing indicator repeat interval. Signal indicators expire after ~5 seconds,
/// so we resend every 4 seconds.
const TYPING_REPEAT_INTERVAL: Duration = Duration::from_secs(4);

/// HTTP connect timeout for the reqwest client.
const HTTP_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Per-request timeout for JSON-RPC calls.
const RPC_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Initial SSE reconnection delay.
const SSE_INITIAL_BACKOFF: Duration = Duration::from_secs(2);

/// Maximum SSE reconnection delay.
const SSE_MAX_BACKOFF: Duration = Duration::from_secs(60);

// ── signal-cli SSE event JSON shapes ────────────────────────────

#[derive(Debug, Deserialize)]
struct SseEnvelope {
    #[serde(default)]
    envelope: Option<Envelope>,
}

#[derive(Debug, Deserialize)]
struct Envelope {
    /// Source identifier (UUID for privacy-enabled users).
    #[serde(default)]
    source: Option<String>,
    /// E.164 phone number of the sender (preferred identifier).
    #[serde(rename = "sourceNumber", default)]
    source_number: Option<String>,
    /// Display name of the sender.
    #[serde(rename = "sourceName", default)]
    source_name: Option<String>,
    /// UUID of the sender.
    #[serde(rename = "sourceUuid", default)]
    source_uuid: Option<String>,
    #[serde(rename = "dataMessage", default)]
    data_message: Option<DataMessage>,
    /// Present when the envelope is a story message (dropped when `ignore_stories` is true).
    #[serde(rename = "storyMessage", default)]
    story_message: Option<serde_json::Value>,
    #[serde(default)]
    timestamp: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct DataMessage {
    #[serde(default)]
    message: Option<String>,
    #[serde(default)]
    timestamp: Option<u64>,
    #[serde(rename = "groupInfo", default)]
    group_info: Option<GroupInfo>,
    /// Inbound attachments as opaque JSON. signal-cli provides file paths on disk
    /// in each attachment object (e.g. `contentType`, `filename`, `file`).
    #[serde(default)]
    attachments: Option<Vec<serde_json::Value>>,
}

#[derive(Debug, Deserialize)]
struct GroupInfo {
    #[serde(rename = "groupId", default)]
    group_id: Option<String>,
}

// ── recipient routing ───────────────────────────────────────────

/// Classification for outbound message routing.
#[derive(Debug, Clone)]
enum RecipientTarget {
    /// Direct message to a phone number or UUID.
    Direct(String),
    /// Group message by group ID.
    Group(String),
}

// ── adapter ─────────────────────────────────────────────────────

/// Signal messaging adapter.
///
/// Connects to a signal-cli JSON-RPC daemon for sending/receiving messages.
/// Implements the [`Messaging`] trait for integration with [`MessagingManager`].
pub struct SignalAdapter {
    /// Runtime key used for registration and routing (e.g. `"signal"` or `"signal:personal"`).
    runtime_key: String,
    /// Base URL of the signal-cli HTTP daemon (e.g. `http://127.0.0.1:8686`).
    http_url: String,
    /// E.164 phone number of the bot's Signal account.
    account: String,
    /// Whether to silently drop story messages.
    ignore_stories: bool,
    /// Hot-reloadable permissions (DM allowlist + group filter).
    permissions: Arc<ArcSwap<SignalPermissions>>,
    /// HTTP client for JSON-RPC and SSE connections.
    client: reqwest::Client,
    /// Directory for temporary outbound attachment files.
    tmp_dir: PathBuf,
    /// Maps conversation_id → reply target string for outbound routing.
    /// The target is either a phone number/UUID (DM) or `"group:{id}"` (group).
    /// Limited to 10,000 entries to prevent unbounded growth.
    reply_targets: Arc<RwLock<LruCache<String, String>>>,
    /// Repeating typing indicator tasks per conversation_id.
    typing_tasks: Arc<RwLock<HashMap<String, JoinHandle<()>>>>,
    /// Shutdown signal for the SSE listener loop.
    shutdown_tx: Arc<RwLock<Option<mpsc::Sender<()>>>>,
}

impl SignalAdapter {
    /// Create a new Signal adapter.
    ///
    /// # Arguments
    /// - `runtime_key` — Adapter name for registration (e.g. `"signal"` or `"signal:work"`).
    /// - `http_url` — Base URL of the signal-cli daemon (e.g. `http://127.0.0.1:8686`).
    /// - `account` — E.164 phone number of the bot's Signal account.
    /// - `ignore_stories` — Whether to silently drop story messages.
    /// - `permissions` — Hot-reloadable permission filters.
    /// - `tmp_dir` — Directory for temporary outbound attachment files.
    pub fn new(
        runtime_key: impl Into<String>,
        http_url: impl Into<String>,
        account: impl Into<String>,
        ignore_stories: bool,
        permissions: Arc<ArcSwap<SignalPermissions>>,
        tmp_dir: PathBuf,
    ) -> Self {
        let client = reqwest::Client::builder()
            .connect_timeout(HTTP_CONNECT_TIMEOUT)
            .build()
            .expect("failed to build reqwest client for signal adapter");

        Self {
            runtime_key: runtime_key.into(),
            http_url: http_url.into(),
            account: account.into(),
            ignore_stories,
            permissions,
            client,
            tmp_dir,
            reply_targets: Arc::new(RwLock::new(LruCache::new(
                NonZeroUsize::new(10_000).expect("10_000 is non-zero"),
            ))),
            typing_tasks: Arc::new(RwLock::new(HashMap::new())),
            shutdown_tx: Arc::new(RwLock::new(None)),
        }
    }

    // ── JSON-RPC ────────────────────────────────────────────────

    /// Send a JSON-RPC request to the signal-cli daemon.
    ///
    /// Returns the `result` field from the response, or `None` if the response
    /// has no body (e.g. 201 for typing indicators).
    async fn rpc_request(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> anyhow::Result<Option<serde_json::Value>> {
        let url = format!("{}{RPC_ENDPOINT}", self.http_url);
        let request_id = Uuid::new_v4().to_string();

        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
            "id": request_id,
        });

        let response = self
            .client
            .post(&url)
            .timeout(RPC_REQUEST_TIMEOUT)
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .with_context(|| format!("signal RPC request to {method} failed"))?;

        // 201 = success with no body (e.g. typing indicators).
        if response.status().as_u16() == 201 {
            return Ok(None);
        }

        // Reject oversized responses before buffering.
        if let Some(length) = response.content_length()
            && length as usize > MAX_RPC_RESPONSE_SIZE
        {
            anyhow::bail!(
                "signal RPC response too large: {length} bytes (max {MAX_RPC_RESPONSE_SIZE})"
            );
        }

        let status = response.status();

        // Stream the response body with size guard.
        let mut stream = response.bytes_stream();
        let mut response_body = Vec::new();
        let mut total_bytes = 0usize;

        while let Some(chunk) = stream.next().await {
            let chunk = chunk.context("failed to read signal RPC response chunk")?;
            total_bytes += chunk.len();
            if total_bytes > MAX_RPC_RESPONSE_SIZE {
                anyhow::bail!(
                    "signal RPC response exceeded {MAX_RPC_RESPONSE_SIZE} bytes while streaming"
                );
            }
            response_body.extend_from_slice(&chunk);
        }

        if !status.is_success() {
            let truncated_length = std::cmp::min(response_body.len(), 512);
            let truncated_body = String::from_utf8_lossy(&response_body[..truncated_length]);
            anyhow::bail!("signal RPC HTTP {}: {truncated_body}", status.as_u16());
        }

        if response_body.is_empty() {
            return Ok(None);
        }

        let parsed: serde_json::Value =
            serde_json::from_slice(&response_body).context("invalid signal RPC response JSON")?;

        if let Some(error) = parsed.get("error") {
            let code = error.get("code").and_then(|c| c.as_i64()).unwrap_or(-1);
            let message = error
                .get("message")
                .and_then(|m| m.as_str())
                .unwrap_or("unknown");
            anyhow::bail!("signal RPC error {code}: {message}");
        }

        Ok(parsed.get("result").cloned())
    }

    /// Build JSON-RPC params for a send or typing call.
    ///
    /// **Critical:** DM `recipient` must be a JSON **array** (signal-cli requirement).
    /// Group messages use `groupId` instead (mutually exclusive with `recipient`).
    fn build_rpc_params(
        &self,
        target: &RecipientTarget,
        message: Option<&str>,
        attachments: Option<&[String]>,
    ) -> serde_json::Value {
        let mut params = match target {
            RecipientTarget::Direct(identifier) => {
                serde_json::json!({
                    "recipient": [identifier],
                    "account": &self.account,
                })
            }
            RecipientTarget::Group(group_id) => {
                serde_json::json!({
                    "groupId": group_id,
                    "account": &self.account,
                })
            }
        };

        if let Some(text) = message {
            params["message"] = serde_json::Value::String(text.to_string());
        }

        if let Some(paths) = attachments
            && !paths.is_empty()
        {
            params["attachments"] = serde_json::Value::Array(
                paths
                    .iter()
                    .map(|path| serde_json::Value::String(path.clone()))
                    .collect(),
            );
        }

        params
    }

    // ── outbound helpers ────────────────────────────────────────

    /// Resolve the outbound target for a conversation from metadata or the
    /// reply target cache.
    fn resolve_target(&self, message: &InboundMessage) -> Option<RecipientTarget> {
        // First try metadata (set during inbound processing).
        if let Some(target_string) = message
            .metadata
            .get("signal_target")
            .and_then(|value| value.as_str())
        {
            return Some(parse_recipient_target(target_string));
        }
        None
    }

    /// Send a text message to the resolved target.
    async fn send_text(&self, target: &RecipientTarget, text: &str) -> anyhow::Result<()> {
        let params = self.build_rpc_params(target, Some(text), None);
        self.rpc_request("send", params).await?;
        Ok(())
    }

    /// Send a file attachment by writing the data to a temp file, sending it via
    /// the JSON-RPC `attachments` field, then cleaning up the temp file.
    async fn send_file(
        &self,
        target: &RecipientTarget,
        filename: &str,
        data: &[u8],
        caption: Option<&str>,
    ) -> anyhow::Result<()> {
        // Ensure tmp dir exists.
        tokio::fs::create_dir_all(&self.tmp_dir)
            .await
            .with_context(|| {
                format!(
                    "failed to create signal tmp dir: {}",
                    self.tmp_dir.display()
                )
            })?;

        // Write temp file with a unique name to avoid collisions.
        // Sanitize filename to prevent path traversal.
        let safe_filename = std::path::Path::new(filename)
            .file_name()
            .and_then(|name| name.to_str())
            .filter(|name| !name.is_empty())
            .unwrap_or("attachment.bin");
        let unique_name = format!("{}_{}", Uuid::new_v4(), safe_filename);
        let tmp_path = self.tmp_dir.join(&unique_name);
        tokio::fs::write(&tmp_path, data).await.with_context(|| {
            format!(
                "failed to write signal attachment to {}",
                tmp_path.display()
            )
        })?;

        let attachment_path = tmp_path
            .to_str()
            .context("signal tmp path is not valid UTF-8")?
            .to_string();

        let result = {
            let params = self.build_rpc_params(target, caption, Some(&[attachment_path]));
            self.rpc_request("send", params).await
        };

        // Clean up temp file regardless of send result.
        if let Err(error) = tokio::fs::remove_file(&tmp_path).await {
            tracing::debug!(
                path = %tmp_path.display(),
                %error,
                "failed to clean up signal attachment temp file"
            );
        }

        result?;
        Ok(())
    }

    /// Cancel the repeating typing indicator for a conversation.
    async fn stop_typing(&self, conversation_id: &str) {
        if let Some(handle) = self.typing_tasks.write().await.remove(conversation_id) {
            handle.abort();
        }
    }

    // ── SSE envelope processing ─────────────────────────────────

    /// Process a single SSE envelope into an `InboundMessage` if it passes
    /// permission filters and contains valid content.
    ///
    /// Returns `(InboundMessage, reply_target_string)` on success.
    fn process_envelope(&self, envelope: &Envelope) -> Option<(InboundMessage, String)> {
        // Drop story messages when configured.
        if self.ignore_stories && envelope.story_message.is_some() {
            tracing::debug!("signal: dropping story message");
            return None;
        }

        let data_message = envelope.data_message.as_ref()?;

        let has_attachments = data_message
            .attachments
            .as_ref()
            .is_some_and(|attachments| !attachments.is_empty());
        let has_text = data_message
            .message
            .as_ref()
            .is_some_and(|text| !text.is_empty());

        // Need either text or attachments to produce a message.
        if !has_text && !has_attachments {
            return None;
        }

        // Use message text, or fall back to "[Attachment]" for attachment-only messages.
        let text = data_message
            .message
            .as_deref()
            .filter(|text| !text.is_empty())
            .map(String::from)
            .or_else(|| {
                if has_attachments {
                    Some("[Attachment]".to_string())
                } else {
                    None
                }
            })?;

        // Resolve sender: prefer sourceNumber (E.164), fall back to source (UUID), then source_uuid.
        let sender = envelope
            .source_number
            .as_deref()
            .or(envelope.source.as_deref())
            .or(envelope.source_uuid.as_deref())
            .map(String::from)?;

        // Determine if this is a group message.
        let group_id = data_message
            .group_info
            .as_ref()
            .and_then(|group| group.group_id.as_deref());
        let is_group = group_id.is_some();

        // ── permission checks ───────────────────────────────────
        let permissions = self.permissions.load();

        if is_group {
            // Group filter: None/empty = block all groups, ["*"] = allow all, specific list = check
            match &permissions.group_filter {
                None => {
                    tracing::info!(
                        group_id = ?group_id,
                        "signal: group message blocked (no groups configured): "
                    );
                    return None;
                }
                Some(allowed_groups) => {
                    if allowed_groups.is_empty() {
                        tracing::info!(
                            group_id = ?group_id,
                            "signal: group message blocked (empty group filter): "
                        );
                        return None;
                    }
                    if allowed_groups.iter().any(|g| g == "*") {
                        // Wildcard: allow all groups
                    } else if let Some(gid) = group_id
                        && !allowed_groups.iter().any(|allowed| allowed == gid)
                    {
                        tracing::info!(
                            group_id = %gid,
                            "signal: group message rejected (not in group filter): "
                        );
                        return None;
                    }
                }
            };
            // Check sender is allowed for groups:
            // Both dm_allowed_users AND group_allowed_users apply to groups (merged)
            // ["*"] in either = allow all, [] = block all, specific list = check
            let all_group_users: Vec<&String> = permissions
                .dm_allowed_users
                .iter()
                .chain(permissions.group_allowed_users.iter())
                .collect();

            let sender_allowed = if all_group_users.is_empty() {
                false // Empty = block all
            } else if all_group_users.iter().any(|u| u.as_str() == "*") {
                true // Wildcard = allow all
            } else {
                all_group_users.iter().any(|allowed| {
                    allowed.as_str() == sender
                        || envelope
                            .source_uuid
                            .as_deref()
                            .is_some_and(|uuid| allowed.as_str() == uuid)
                })
            };
            if !sender_allowed {
                tracing::info!(
                    sender = %sender,
                    "signal: group message rejected (sender not in allowed users): "
                );
                return None;
            }
        } else {
            // DM filter: ["*"] = allow all, [] = block all, specific list = check
            let sender_allowed = if permissions.dm_allowed_users.is_empty() {
                false // Empty = block all
            } else if permissions
                .dm_allowed_users
                .iter()
                .any(|u| u.as_str() == "*")
            {
                true // Wildcard = allow all
            } else {
                permissions.dm_allowed_users.iter().any(|allowed| {
                    // Match against phone number, UUID, or source field.
                    allowed.as_str() == sender
                        || envelope
                            .source_uuid
                            .as_deref()
                            .is_some_and(|uuid| allowed.as_str() == uuid)
                })
            };
            if !sender_allowed {
                tracing::info!(
                    sender = %sender,
                    "signal: DM rejected (sender not in dm_allowed_users): "
                );
                return None;
            }
        }

        // Log authorized message
        let sender_display = envelope
            .source_uuid
            .as_deref()
            .or(envelope.source_number.as_deref())
            .unwrap_or(&sender);
        tracing::info!(
            sender = %sender_display,
            text_len = %text.len(),
            is_group = is_group,
            "signal: message received"
        );

        // ── build reply target ──────────────────────────────────

        let reply_target = if let Some(gid) = group_id {
            format!("{GROUP_TARGET_PREFIX}{gid}")
        } else {
            sender.clone()
        };

        // ── build conversation ID ───────────────────────────────

        let base_conversation_id = if let Some(gid) = group_id {
            format!("signal:group:{gid}")
        } else {
            // Use UUID if available (stable), fall back to phone number.
            let identifier = envelope.source_uuid.as_deref().unwrap_or(&sender);
            format!("signal:{identifier}")
        };
        let conversation_id =
            apply_runtime_adapter_to_conversation_id(&self.runtime_key, base_conversation_id);

        // ── build timestamp ─────────────────────────────────────

        let timestamp_millis = data_message
            .timestamp
            .or(envelope.timestamp)
            .unwrap_or_else(|| {
                u64::try_from(
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_millis(),
                )
                .unwrap_or(u64::MAX)
            });

        let timestamp = chrono::DateTime::from_timestamp_millis(timestamp_millis as i64)
            .unwrap_or_else(chrono::Utc::now);

        // ── build metadata ──────────────────────────────────────

        let (metadata, formatted_author) = build_metadata(envelope, &reply_target, group_id);

        // ── build sender_id ─────────────────────────────────────

        let sender_id = envelope
            .source_uuid
            .as_deref()
            .unwrap_or(&sender)
            .to_string();

        // ── build message ID ────────────────────────────────────

        let message_id = format!("{timestamp_millis}_{sender_id}");

        // ── assemble InboundMessage ─────────────────────────────

        let inbound = InboundMessage {
            id: message_id,
            source: "signal".into(),
            adapter: Some(self.runtime_key.clone()),
            conversation_id,
            sender_id,
            agent_id: None,
            content: MessageContent::Text(text),
            timestamp,
            metadata,
            formatted_author,
        };

        Some((inbound, reply_target))
    }
}

// ── Messaging trait implementation ──────────────────────────────

impl Messaging for SignalAdapter {
    fn name(&self) -> &str {
        &self.runtime_key
    }

    async fn start(&self) -> crate::Result<InboundStream> {
        let (inbound_tx, inbound_rx) = mpsc::channel(256);
        let (shutdown_tx, shutdown_rx) = mpsc::channel::<()>(1);

        *self.shutdown_tx.write().await = Some(shutdown_tx);

        // Verify connectivity before spawning the listener.
        self.health_check().await?;

        tracing::info!(
            adapter = %self.runtime_key,
            account = %self.account,
            "signal adapter connected"
        );

        // Clone fields for the spawned SSE listener task.
        let client = self.client.clone();
        let http_url = self.http_url.clone();
        let account = self.account.clone();
        let runtime_key = self.runtime_key.clone();
        let ignore_stories = self.ignore_stories;
        let permissions = self.permissions.clone();
        let reply_targets = self.reply_targets.clone();
        let tmp_dir = self.tmp_dir.clone();

        tokio::spawn(async move {
            // Build a temporary adapter for envelope processing inside the task.
            // We need access to process_envelope which reads permissions.
            let adapter = SignalAdapter {
                runtime_key,
                http_url: http_url.clone(),
                account: account.clone(),
                ignore_stories,
                permissions,
                client: client.clone(),
                tmp_dir,
                reply_targets: reply_targets.clone(),
                typing_tasks: Arc::new(RwLock::new(HashMap::new())),
                shutdown_tx: Arc::new(RwLock::new(None)),
            };

            sse_listener(
                adapter,
                client,
                http_url,
                account,
                inbound_tx,
                reply_targets,
                shutdown_rx,
            )
            .await;
        });

        let stream = tokio_stream::wrappers::ReceiverStream::new(inbound_rx);
        Ok(Box::pin(stream))
    }

    async fn respond(
        &self,
        message: &InboundMessage,
        response: OutboundResponse,
    ) -> crate::Result<()> {
        let target = self.resolve_target(message).with_context(|| {
            format!(
                "cannot resolve signal reply target for conversation {}",
                message.conversation_id
            )
        })?;

        match response {
            OutboundResponse::Text(text) => {
                self.stop_typing(&message.conversation_id).await;
                self.send_text(&target, &text).await?;
            }
            OutboundResponse::RichMessage { text, .. } => {
                // Signal has no rich formatting — send plain text.
                self.stop_typing(&message.conversation_id).await;
                self.send_text(&target, &text).await?;
            }
            OutboundResponse::ThreadReply { text, .. } => {
                // Signal has no named threads — send as regular message.
                self.stop_typing(&message.conversation_id).await;
                self.send_text(&target, &text).await?;
            }
            OutboundResponse::File {
                filename,
                data,
                caption,
                ..
            } => {
                self.stop_typing(&message.conversation_id).await;
                self.send_file(&target, &filename, &data, caption.as_deref())
                    .await?;
            }
            OutboundResponse::Reaction(_) => {
                // Signal supports reactions via JSON-RPC but the API shape is complex
                // (requires target author UUID + timestamp). Skip for now.
                tracing::debug!(
                    conversation_id = %message.conversation_id,
                    "signal: reactions not supported, dropping"
                );
            }
            OutboundResponse::RemoveReaction(_) => {
                tracing::debug!(
                    conversation_id = %message.conversation_id,
                    "signal: remove reactions not supported, dropping"
                );
            }
            OutboundResponse::Ephemeral { text, .. } => {
                // Signal has no ephemeral messages — send as regular text.
                self.stop_typing(&message.conversation_id).await;
                self.send_text(&target, &text).await?;
            }
            OutboundResponse::ScheduledMessage { text, .. } => {
                // Signal has no scheduled messages — send immediately.
                self.stop_typing(&message.conversation_id).await;
                self.send_text(&target, &text).await?;
            }
            OutboundResponse::StreamStart
            | OutboundResponse::StreamChunk(_)
            | OutboundResponse::StreamEnd => {
                // Signal can't edit sent messages — streaming is not supported.
                // StreamStart/Chunk/End are no-ops.
            }
            OutboundResponse::Status(status) => {
                self.send_status(message, status).await?;
            }
        }

        Ok(())
    }

    async fn send_status(
        &self,
        message: &InboundMessage,
        status: StatusUpdate,
    ) -> crate::Result<()> {
        match status {
            StatusUpdate::Thinking => {
                let Some(target) = self.resolve_target(message) else {
                    return Ok(());
                };

                // Abort any existing typing task before starting a new one.
                self.stop_typing(&message.conversation_id).await;

                let client = self.client.clone();
                let http_url = self.http_url.clone();
                let account = self.account.clone();
                let conversation_id = message.conversation_id.clone();
                let rpc_url = format!("{http_url}{RPC_ENDPOINT}");

                // Send typing indicator immediately, then repeat every 4 seconds.
                let handle = tokio::spawn(async move {
                    loop {
                        let params = match &target {
                            RecipientTarget::Direct(identifier) => {
                                serde_json::json!({
                                    "recipient": [identifier],
                                    "account": &account,
                                })
                            }
                            RecipientTarget::Group(group_id) => {
                                serde_json::json!({
                                    "groupId": group_id,
                                    "account": &account,
                                })
                            }
                        };

                        let body = serde_json::json!({
                            "jsonrpc": "2.0",
                            "method": "sendTyping",
                            "params": params,
                            "id": Uuid::new_v4().to_string(),
                        });

                        if let Err(error) = client
                            .post(&rpc_url)
                            .timeout(RPC_REQUEST_TIMEOUT)
                            .header("Content-Type", "application/json")
                            .json(&body)
                            .send()
                            .await
                        {
                            tracing::debug!(%error, "failed to send signal typing indicator");
                            break;
                        }

                        tokio::time::sleep(TYPING_REPEAT_INTERVAL).await;
                    }
                });

                self.typing_tasks
                    .write()
                    .await
                    .insert(conversation_id, handle);
            }
            _ => {
                self.stop_typing(&message.conversation_id).await;
            }
        }

        Ok(())
    }

    async fn health_check(&self) -> crate::Result<()> {
        let url = format!("{}{HEALTH_ENDPOINT}", self.http_url);
        tracing::debug!(url = %url, "Signal health check: GET");
        let response = self
            .client
            .get(&url)
            .timeout(Duration::from_secs(10))
            .send()
            .await
            .context("signal health check failed: connection error")?;

        tracing::debug!(status = %response.status(), "Signal health check response");

        if response.status().is_success() {
            Ok(())
        } else {
            Err(anyhow::anyhow!(
                "signal health check returned HTTP {}",
                response.status()
            ))?
        }
    }

    async fn shutdown(&self) -> crate::Result<()> {
        // Cancel all typing indicator tasks.
        let mut tasks = self.typing_tasks.write().await;
        for (_, handle) in tasks.drain() {
            handle.abort();
        }

        // Signal the SSE listener to stop.
        if let Some(shutdown_tx) = self.shutdown_tx.read().await.as_ref() {
            shutdown_tx.send(()).await.ok();
        }

        tracing::info!(adapter = %self.runtime_key, "signal adapter shut down");
        Ok(())
    }
}

// ── SSE listener ────────────────────────────────────────────────

/// Long-running SSE listener that reconnects with exponential backoff.
///
/// Connects to `{http_url}/api/v1/events?account={account}`, parses SSE events,
/// processes envelopes through the adapter's permission filters, and sends
/// valid inbound messages through the channel.
async fn sse_listener(
    adapter: SignalAdapter,
    client: reqwest::Client,
    http_url: String,
    account: String,
    inbound_tx: mpsc::Sender<InboundMessage>,
    reply_targets: Arc<RwLock<LruCache<String, String>>>,
    mut shutdown_rx: mpsc::Receiver<()>,
) {
    let sse_url = match reqwest::Url::parse(&format!("{http_url}{SSE_ENDPOINT}")) {
        Ok(mut url) => {
            url.query_pairs_mut().append_pair("account", &account);
            url
        }
        Err(error) => {
            tracing::error!(%error, "signal: invalid SSE URL, listener exiting");
            return;
        }
    };

    let mut retry_delay = SSE_INITIAL_BACKOFF;

    loop {
        // Check for shutdown before each connection attempt.
        if shutdown_rx.try_recv().is_ok() {
            tracing::info!("signal SSE listener shutting down");
            return;
        }

        let response = client
            .get(sse_url.clone())
            .header("Accept", "text/event-stream")
            .send()
            .await;

        let response = match response {
            Ok(response) if response.status().is_success() => response,
            Ok(response) => {
                tracing::warn!(
                    status = %response.status(),
                    "signal SSE returned error, retrying in {:?}",
                    retry_delay
                );
                tokio::time::sleep(retry_delay).await;
                retry_delay = (retry_delay * 2).min(SSE_MAX_BACKOFF);
                continue;
            }
            Err(error) => {
                tracing::warn!(
                    %error,
                    "signal SSE connect error, retrying in {:?}",
                    retry_delay
                );
                tokio::time::sleep(retry_delay).await;
                retry_delay = (retry_delay * 2).min(SSE_MAX_BACKOFF);
                continue;
            }
        };

        // Connection succeeded — reset backoff.
        retry_delay = SSE_INITIAL_BACKOFF;
        tracing::info!("signal SSE connected");

        // ── stream processing loop ──────────────────────────────

        let mut bytes_stream = response.bytes_stream();
        let mut buffer = String::with_capacity(8192);
        let mut current_data = String::with_capacity(4096);
        // Holds trailing bytes from the previous chunk that form an incomplete
        // multi-byte UTF-8 sequence. At most 3 bytes (the longest incomplete
        // leading sequence for a 4-byte character).
        let mut utf8_carry: Vec<u8> = Vec::with_capacity(4);

        'stream: loop {
            tokio::select! {
                _ = shutdown_rx.recv() => {
                    tracing::info!("signal SSE listener shutting down");
                    return;
                }
                chunk = bytes_stream.next() => {
                    let Some(chunk) = chunk else {
                        // Stream ended — break to reconnect.
                        break 'stream;
                    };
                    let chunk = match chunk {
                        Ok(chunk) => chunk,
                        Err(error) => {
                            tracing::debug!(%error, "signal SSE chunk error, reconnecting");
                            break 'stream;
                        }
                    };

                    // Prepend any leftover bytes from the previous chunk.
                    let decode_buf = if utf8_carry.is_empty() {
                        chunk.to_vec()
                    } else {
                        let mut combined = std::mem::take(&mut utf8_carry);
                        combined.extend_from_slice(&chunk);
                        combined
                    };

                    // Decode as much valid UTF-8 as possible, carrying over any
                    // incomplete trailing sequence to the next iteration.
                    let (valid_len, carry_start) = match std::str::from_utf8(&decode_buf) {
                        Ok(_) => (decode_buf.len(), decode_buf.len()),
                        Err(error) => {
                            let valid_up_to = error.valid_up_to();
                            match error.error_len() {
                                Some(bad_len) => {
                                    // Genuinely invalid byte sequence — skip the bad byte(s).
                                    tracing::debug!(
                                        offset = valid_up_to,
                                        "signal SSE: invalid UTF-8 byte, skipping"
                                    );
                                    (valid_up_to, valid_up_to + bad_len)
                                }
                                None => {
                                    // Incomplete multi-byte sequence at the end — carry it over.
                                    (valid_up_to, valid_up_to)
                                }
                            }
                        }
                    };

                    let text = match std::str::from_utf8(&decode_buf[..valid_len]) {
                        Ok(s) => s,
                        Err(_) => {
                            tracing::warn!(
                                "signal SSE: unexpected invalid UTF-8 at boundary, skipping chunk"
                            );
                            continue;
                        }
                    };

                    // Buffer overflow protection.
                    if buffer.len() + text.len() > MAX_SSE_BUFFER_SIZE {
                        tracing::warn!(
                            buffer_len = buffer.len(),
                            text_len = text.len(),
                            "signal SSE buffer overflow, resetting"
                        );
                        buffer.clear();
                        utf8_carry.clear();
                        current_data.clear();
                        continue;
                    }
                    buffer.push_str(text);

                    // Preserve any trailing incomplete bytes for the next chunk.
                    if carry_start < decode_buf.len() {
                        utf8_carry.extend_from_slice(&decode_buf[carry_start..]);
                    }

                    // Parse complete lines from the buffer.
                    while let Some(newline_pos) = buffer.find('\n') {
                        let line = buffer[..newline_pos].trim_end_matches('\r').to_string();
                        buffer.drain(..=newline_pos);

                        // Skip SSE comments (keepalive pings).
                        if line.starts_with(':') {
                            continue;
                        }

                        if line.is_empty() {
                            // Empty line = event boundary — dispatch accumulated data.
                            if !current_data.is_empty() {
                                process_sse_event(
                                    &adapter,
                                    &current_data,
                                    &inbound_tx,
                                    &reply_targets,
                                ).await;
                                current_data.clear();
                            }
                        } else if let Some(data) = line.strip_prefix("data:") {
                            // Guard against oversized single events.
                            if current_data.len() + data.len() > MAX_SSE_EVENT_SIZE {
                                tracing::warn!("signal SSE event too large, dropping");
                                current_data.clear();
                                continue;
                            }
                            if !current_data.is_empty() {
                                current_data.push('\n');
                            }
                            current_data.push_str(data.trim_start());
                        }
                        // Ignore "event:", "id:", "retry:" lines.
                    }
                }
            }
        }

        // Process any trailing data before reconnect.
        if !current_data.is_empty() {
            process_sse_event(&adapter, &current_data, &inbound_tx, &reply_targets).await;
        }

        tracing::debug!("signal SSE stream ended, reconnecting with backoff...");
        tokio::time::sleep(retry_delay).await;
        retry_delay = (retry_delay * 2).min(SSE_MAX_BACKOFF);
    }
}

/// Parse and dispatch a single SSE event.
async fn process_sse_event(
    adapter: &SignalAdapter,
    data: &str,
    inbound_tx: &mpsc::Sender<InboundMessage>,
    reply_targets: &Arc<RwLock<LruCache<String, String>>>,
) {
    let sse: SseEnvelope = match serde_json::from_str(data) {
        Ok(sse) => sse,
        Err(error) => {
            tracing::debug!(%error, "signal SSE: failed to parse event, skipping");
            return;
        }
    };

    let Some(ref envelope) = sse.envelope else {
        return;
    };

    let Some((inbound, reply_target)) = adapter.process_envelope(envelope) else {
        return;
    };

    // Cache the reply target so outbound routing works for this conversation.
    reply_targets
        .write()
        .await
        .put(inbound.conversation_id.clone(), reply_target);

    if let Err(error) = inbound_tx.send(inbound).await {
        tracing::warn!(%error, "signal: inbound channel closed (receiver dropped)");
    }
}

// ── metadata builder ────────────────────────────────────────────

/// Build platform-specific metadata for a Signal message.
///
/// Returns `(metadata_map, formatted_author)`.
fn build_metadata(
    envelope: &Envelope,
    reply_target: &str,
    group_id: Option<&str>,
) -> (HashMap<String, serde_json::Value>, Option<String>) {
    let mut metadata = HashMap::new();

    // Sender identifiers.
    let sender = envelope
        .source_number
        .as_deref()
        .or(envelope.source.as_deref())
        .unwrap_or("unknown");

    metadata.insert(
        "signal_source".into(),
        serde_json::Value::String(sender.to_string()),
    );

    // Always include both UUID and phone number (null if not available)
    metadata.insert(
        "signal_source_number".into(),
        envelope
            .source_number
            .clone()
            .map(serde_json::Value::String)
            .unwrap_or(serde_json::Value::Null),
    );

    metadata.insert(
        "signal_source_uuid".into(),
        envelope
            .source_uuid
            .clone()
            .map(serde_json::Value::String)
            .unwrap_or(serde_json::Value::Null),
    );

    // Sender context for agent visibility - always shows both e164 and uuid
    let e164 = envelope
        .source_number
        .as_deref()
        .map(|n| format!("e164:{}", n))
        .unwrap_or_else(|| "e164:none".to_string());

    let uuid = envelope
        .source_uuid
        .as_deref()
        .map(|u| format!("uuid:{}", u))
        .unwrap_or_else(|| "uuid:none".to_string());

    let sender_context = format!("[Signal: {} {}]", e164, uuid);
    metadata.insert(
        "sender_context".into(),
        serde_json::Value::String(sender_context),
    );

    if let Some(ref source_name) = envelope.source_name {
        metadata.insert(
            "signal_source_name".into(),
            serde_json::Value::String(source_name.clone()),
        );
    }

    // Reply target for outbound routing.
    metadata.insert(
        "signal_target".into(),
        serde_json::Value::String(reply_target.to_string()),
    );

    // Timestamp.
    let timestamp = envelope
        .data_message
        .as_ref()
        .and_then(|dm| dm.timestamp)
        .or(envelope.timestamp);
    if let Some(ts) = timestamp {
        metadata.insert(
            "signal_timestamp".into(),
            serde_json::Value::Number(ts.into()),
        );
    }

    // Chat type.
    let chat_type = if group_id.is_some() { "group" } else { "dm" };
    metadata.insert(
        "signal_chat_type".into(),
        serde_json::Value::String(chat_type.into()),
    );

    // Group ID.
    if let Some(gid) = group_id {
        metadata.insert(
            "signal_group_id".into(),
            serde_json::Value::String(gid.to_string()),
        );
    }

    // Standard metadata keys.
    metadata.insert(
        metadata_keys::MESSAGE_ID.into(),
        serde_json::Value::String(format!("{}", timestamp.unwrap_or(0))),
    );

    // Channel name: "Group Chat {id}" for groups, "Direct Message with {name}" for DMs.
    let channel_name = if let Some(gid) = group_id {
        format!("Group Chat {}", gid)
    } else {
        envelope
            .source_name
            .as_deref()
            .filter(|name| !name.is_empty())
            .map(|name| format!("Direct Message with {}", name))
            .unwrap_or_else(|| "Direct Message".to_string())
    };
    metadata.insert(
        metadata_keys::CHANNEL_NAME.into(),
        serde_json::Value::String(channel_name),
    );

    // Sender display name.
    let display_name = envelope
        .source_name
        .as_deref()
        .filter(|name| !name.is_empty())
        .unwrap_or(sender);

    metadata.insert(
        "sender_display_name".into(),
        serde_json::Value::String(display_name.to_string()),
    );

    // Formatted author: "Display Name (+phone)" or just the display name.
    let formatted_author = if let Some(ref source_number) = envelope.source_number
        && display_name != source_number
    {
        Some(format!("{display_name} ({source_number})"))
    } else {
        Some(display_name.to_string())
    };

    (metadata, formatted_author)
}

// ── helper functions ────────────────────────────────────────────

/// Parse a reply target string into a `RecipientTarget`.
///
/// Format: `"group:{group_id}"` for groups, otherwise treated as a direct
/// message identifier (phone number or UUID).
fn parse_recipient_target(target: &str) -> RecipientTarget {
    if let Some(group_id) = target.strip_prefix(GROUP_TARGET_PREFIX) {
        RecipientTarget::Group(group_id.to_string())
    } else {
        RecipientTarget::Direct(target.to_string())
    }
}

// ── tests ───────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_recipient_target_dm() {
        let target = parse_recipient_target("+1234567890");
        assert!(matches!(target, RecipientTarget::Direct(ref id) if id == "+1234567890"));
    }

    #[test]
    fn parse_recipient_target_uuid() {
        let target = parse_recipient_target("a1b2c3d4-e5f6-7890-abcd-ef1234567890");
        assert!(matches!(
            target,
            RecipientTarget::Direct(ref id) if id == "a1b2c3d4-e5f6-7890-abcd-ef1234567890"
        ));
    }

    #[test]
    fn parse_recipient_target_group() {
        let target = parse_recipient_target("group:abc123");
        assert!(matches!(target, RecipientTarget::Group(ref id) if id == "abc123"));
    }

    #[test]
    fn parse_sse_envelope_dm() {
        let json = r#"{
            "envelope": {
                "sourceNumber": "+1111111111",
                "sourceName": "Alice",
                "sourceUuid": "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee",
                "dataMessage": {
                    "message": "Hello",
                    "timestamp": 1700000000000
                }
            }
        }"#;
        let sse: SseEnvelope = serde_json::from_str(json).unwrap();
        let envelope = sse.envelope.unwrap();
        assert_eq!(envelope.source_number.as_deref(), Some("+1111111111"));
        assert_eq!(envelope.source_name.as_deref(), Some("Alice"));
        let dm = envelope.data_message.unwrap();
        assert_eq!(dm.message.as_deref(), Some("Hello"));
        assert_eq!(dm.timestamp, Some(1700000000000));
        assert!(dm.group_info.is_none());
    }

    #[test]
    fn parse_sse_envelope_group() {
        let json = r#"{
            "envelope": {
                "sourceNumber": "+1111111111",
                "dataMessage": {
                    "message": "Group hello",
                    "groupInfo": {
                        "groupId": "testgroup123"
                    }
                }
            }
        }"#;
        let sse: SseEnvelope = serde_json::from_str(json).unwrap();
        let dm = sse.envelope.unwrap().data_message.unwrap();
        assert_eq!(
            dm.group_info.unwrap().group_id.as_deref(),
            Some("testgroup123")
        );
    }

    #[test]
    fn parse_sse_envelope_story_message() {
        let json = r#"{
            "envelope": {
                "sourceNumber": "+1111111111",
                "storyMessage": {"text": "story content"}
            }
        }"#;
        let sse: SseEnvelope = serde_json::from_str(json).unwrap();
        assert!(sse.envelope.unwrap().story_message.is_some());
    }

    #[test]
    fn parse_sse_envelope_with_attachments() {
        let json = r#"{
            "envelope": {
                "sourceNumber": "+1111111111",
                "dataMessage": {
                    "message": "See attached",
                    "attachments": [
                        {"contentType": "image/jpeg", "filename": "photo.jpg"},
                        {"contentType": "application/pdf"}
                    ]
                }
            }
        }"#;
        let sse: SseEnvelope = serde_json::from_str(json).unwrap();
        let dm = sse.envelope.unwrap().data_message.unwrap();
        assert_eq!(dm.attachments.unwrap().len(), 2);
    }

    #[test]
    fn parse_sse_envelope_empty() {
        let json = r#"{}"#;
        let sse: SseEnvelope = serde_json::from_str(json).unwrap();
        assert!(sse.envelope.is_none());
    }

    #[test]
    fn parse_sse_envelope_no_data_message() {
        let json = r#"{"envelope": {"sourceNumber": "+1111111111"}}"#;
        let sse: SseEnvelope = serde_json::from_str(json).unwrap();
        assert!(sse.envelope.unwrap().data_message.is_none());
    }

    #[test]
    fn build_rpc_params_dm() {
        let adapter = test_adapter();
        let target = RecipientTarget::Direct("+5555555555".to_string());
        let params = adapter.build_rpc_params(&target, Some("hello"), None);

        assert_eq!(params["recipient"], serde_json::json!(["+5555555555"]));
        assert_eq!(params["account"], "+0000000000");
        assert_eq!(params["message"], "hello");
        assert!(params.get("groupId").is_none());
    }

    #[test]
    fn build_rpc_params_group() {
        let adapter = test_adapter();
        let target = RecipientTarget::Group("abc123".to_string());
        let params = adapter.build_rpc_params(&target, Some("hello"), None);

        assert_eq!(params["groupId"], "abc123");
        assert_eq!(params["account"], "+0000000000");
        assert!(params.get("recipient").is_none());
    }

    #[test]
    fn build_rpc_params_with_attachments() {
        let adapter = test_adapter();
        let target = RecipientTarget::Direct("+5555555555".to_string());
        let paths = vec!["/tmp/file.png".to_string()];
        let params = adapter.build_rpc_params(&target, Some("caption"), Some(&paths));

        assert_eq!(params["attachments"], serde_json::json!(["/tmp/file.png"]));
        assert_eq!(params["message"], "caption");
    }

    #[test]
    fn build_rpc_params_no_message() {
        let adapter = test_adapter();
        let target = RecipientTarget::Direct("+5555555555".to_string());
        let params = adapter.build_rpc_params(&target, None, None);

        assert!(params.get("message").is_none());
        assert!(params.get("attachments").is_none());
    }

    #[test]
    fn build_metadata_dm() {
        let envelope = Envelope {
            source: None,
            source_number: Some("+1234567890".into()),
            source_name: Some("Alice".into()),
            source_uuid: Some("aaaa-bbbb".into()),
            data_message: Some(DataMessage {
                message: Some("hi".into()),
                timestamp: Some(1700000000000),
                group_info: None,
                attachments: None,
            }),
            story_message: None,
            timestamp: None,
        };

        let (metadata, author) = build_metadata(&envelope, "+1234567890", None);

        assert_eq!(
            metadata.get("signal_source").unwrap().as_str().unwrap(),
            "+1234567890"
        );
        assert_eq!(
            metadata
                .get("signal_source_uuid")
                .unwrap()
                .as_str()
                .unwrap(),
            "aaaa-bbbb"
        );
        assert_eq!(
            metadata.get("signal_chat_type").unwrap().as_str().unwrap(),
            "dm"
        );
        assert!(metadata.get("signal_group_id").is_none());
        assert_eq!(author.unwrap(), "Alice (+1234567890)");
        assert_eq!(
            metadata.get("sender_context").unwrap().as_str().unwrap(),
            "[Signal: e164:+1234567890 uuid:aaaa-bbbb]"
        );
    }

    #[test]
    fn build_metadata_group() {
        let envelope = Envelope {
            source: None,
            source_number: Some("+1234567890".into()),
            source_name: Some("Bob".into()),
            source_uuid: None,
            data_message: Some(DataMessage {
                message: Some("group msg".into()),
                timestamp: Some(1700000000000),
                group_info: Some(GroupInfo {
                    group_id: Some("grp123".into()),
                }),
                attachments: None,
            }),
            story_message: None,
            timestamp: None,
        };

        let (metadata, _) = build_metadata(&envelope, "group:grp123", Some("grp123"));

        assert_eq!(
            metadata.get("signal_chat_type").unwrap().as_str().unwrap(),
            "group"
        );
        assert_eq!(
            metadata.get("signal_group_id").unwrap().as_str().unwrap(),
            "grp123"
        );
        assert_eq!(
            metadata.get("sender_context").unwrap().as_str().unwrap(),
            "[Signal: e164:+1234567890 uuid:none]"
        );
    }

    #[test]
    fn build_metadata_privacy_mode() {
        let envelope = Envelope {
            source: None,
            source_number: None,
            source_name: Some("Alice".into()),
            source_uuid: Some("uuid-only-123".into()),
            data_message: Some(DataMessage {
                message: Some("hi from privacy mode".into()),
                timestamp: Some(1700000000000),
                group_info: None,
                attachments: None,
            }),
            story_message: None,
            timestamp: None,
        };

        let (metadata, _) = build_metadata(&envelope, "+1234567890", None);

        assert_eq!(
            metadata.get("sender_context").unwrap().as_str().unwrap(),
            "[Signal: e164:none uuid:uuid-only-123]"
        );
    }

    #[test]
    fn process_envelope_drops_story_when_configured() {
        let adapter = test_adapter();
        let envelope = Envelope {
            source: None,
            source_number: Some("+1111111111".into()),
            source_name: None,
            source_uuid: None,
            data_message: None,
            story_message: Some(serde_json::json!({"text": "story"})),
            timestamp: None,
        };
        assert!(adapter.process_envelope(&envelope).is_none());
    }

    #[test]
    fn process_envelope_drops_empty_message() {
        let adapter = test_adapter();
        let envelope = Envelope {
            source: None,
            source_number: Some("+1111111111".into()),
            source_name: None,
            source_uuid: None,
            data_message: Some(DataMessage {
                message: None,
                timestamp: None,
                group_info: None,
                attachments: None,
            }),
            story_message: None,
            timestamp: None,
        };
        assert!(adapter.process_envelope(&envelope).is_none());
    }

    #[test]
    fn process_envelope_attachment_only_produces_placeholder() {
        let adapter = test_adapter_open_dm();
        let envelope = Envelope {
            source: None,
            source_number: Some("+2222222222".into()),
            source_name: None,
            source_uuid: None,
            data_message: Some(DataMessage {
                message: None,
                timestamp: Some(1700000000000),
                group_info: None,
                attachments: Some(vec![serde_json::json!({"contentType": "image/png"})]),
            }),
            story_message: None,
            timestamp: None,
        };
        let result = adapter.process_envelope(&envelope);
        assert!(result.is_some());
        let (msg, _) = result.unwrap();
        if let MessageContent::Text(text) = &msg.content {
            assert_eq!(text, "[Attachment]");
        } else {
            panic!("expected Text content");
        }
    }

    #[test]
    fn process_envelope_dm_allowed() {
        let adapter = test_adapter_with_dm_allowed(vec!["+2222222222".to_string()]);
        let envelope = make_dm_envelope("+2222222222", "hello");
        let result = adapter.process_envelope(&envelope);
        assert!(result.is_some());
    }

    #[test]
    fn process_envelope_dm_rejected() {
        let adapter = test_adapter_with_dm_allowed(vec!["+3333333333".to_string()]);
        let envelope = make_dm_envelope("+2222222222", "hello");
        assert!(adapter.process_envelope(&envelope).is_none());
    }

    #[test]
    fn process_envelope_dm_blocked_when_empty() {
        // Empty dm_allowed_users = block all DMs
        let adapter = test_adapter();
        let envelope = make_dm_envelope("+2222222222", "hello");
        assert!(adapter.process_envelope(&envelope).is_none());
    }

    #[test]
    fn process_envelope_dm_allowed_wildcard() {
        // ["*"] in dm_allowed_users = allow all DMs
        let adapter = test_adapter_open_dm();
        let envelope = make_dm_envelope("+9999999999", "hello from anyone");
        let result = adapter.process_envelope(&envelope);
        assert!(result.is_some());
    }

    #[test]
    fn process_envelope_group_blocked_by_default() {
        // Default group_filter is None = block all groups.
        let adapter = test_adapter();
        let envelope = make_group_envelope("+1111111111", "hi group", "grp123");
        assert!(adapter.process_envelope(&envelope).is_none());
    }

    #[test]
    fn process_envelope_group_blocked_when_empty() {
        // Empty group_filter = block all groups
        let perms = SignalPermissions {
            group_filter: Some(vec![]),
            dm_allowed_users: vec!["*".to_string()],
            group_allowed_users: vec!["*".to_string()],
        };
        let adapter = test_adapter_with_permissions(perms);
        let envelope = make_group_envelope("+1111111111", "hi group", "grp123");
        assert!(adapter.process_envelope(&envelope).is_none());
    }

    #[test]
    fn process_envelope_group_allowed_wildcard() {
        // ["*"] in group_filter = allow all groups
        let perms = SignalPermissions {
            group_filter: Some(vec!["*".to_string()]),
            dm_allowed_users: vec!["*".to_string()],
            group_allowed_users: vec!["*".to_string()],
        };
        let adapter = test_adapter_with_permissions(perms);
        let envelope = make_group_envelope("+1111111111", "hi group", "any-group");
        let result = adapter.process_envelope(&envelope);
        assert!(result.is_some());
    }

    #[test]
    fn process_envelope_group_allowed_when_configured() {
        let perms = SignalPermissions {
            group_filter: Some(vec!["grp123".to_string()]),
            dm_allowed_users: vec!["+1111111111".to_string()],
            group_allowed_users: vec!["+1111111111".to_string()],
        };
        let adapter = test_adapter_with_permissions(perms);
        let envelope = make_group_envelope("+1111111111", "hi group", "grp123");
        let result = adapter.process_envelope(&envelope);
        assert!(result.is_some());
        let (msg, target) = result.unwrap();
        assert_eq!(target, "group:grp123");
        assert!(msg.conversation_id.starts_with("signal:group:grp123"));
    }

    #[test]
    fn process_envelope_conversation_id_dm() {
        let adapter = test_adapter_open_dm();
        let envelope = Envelope {
            source: None,
            source_number: Some("+1234567890".into()),
            source_name: None,
            source_uuid: Some("uuid-1234".into()),
            data_message: Some(DataMessage {
                message: Some("test".into()),
                timestamp: Some(1700000000000),
                group_info: None,
                attachments: None,
            }),
            story_message: None,
            timestamp: None,
        };
        let (msg, _) = adapter.process_envelope(&envelope).unwrap();
        // Should use UUID when available.
        assert_eq!(msg.conversation_id, "signal:uuid-1234");
    }

    // ── test helpers ────────────────────────────────────────────

    fn test_adapter() -> SignalAdapter {
        // Default: blocks all DMs and groups (empty lists)
        test_adapter_with_permissions(SignalPermissions::default())
    }

    fn test_adapter_open_dm() -> SignalAdapter {
        // Wildcard ["*"] means allow all DMs (but groups still blocked by default)
        test_adapter_with_permissions(SignalPermissions {
            group_filter: None,
            dm_allowed_users: vec!["*".to_string()],
            group_allowed_users: vec![],
        })
    }

    fn test_adapter_with_dm_allowed(users: Vec<String>) -> SignalAdapter {
        test_adapter_with_permissions(SignalPermissions {
            group_filter: None,
            dm_allowed_users: users,
            group_allowed_users: vec![],
        })
    }

    fn test_adapter_with_permissions(perms: SignalPermissions) -> SignalAdapter {
        SignalAdapter {
            runtime_key: "signal".into(),
            http_url: "http://127.0.0.1:8686".into(),
            account: "+0000000000".into(),
            ignore_stories: true,
            permissions: Arc::new(ArcSwap::from_pointee(perms)),
            client: reqwest::Client::new(),
            tmp_dir: PathBuf::from("/tmp/spacebot-test"),
            reply_targets: Arc::new(RwLock::new(LruCache::new(
                NonZeroUsize::new(10_000).expect("10_000 is non-zero"),
            ))),
            typing_tasks: Arc::new(RwLock::new(HashMap::new())),
            shutdown_tx: Arc::new(RwLock::new(None)),
        }
    }

    fn make_dm_envelope(sender: &str, text: &str) -> Envelope {
        Envelope {
            source: None,
            source_number: Some(sender.into()),
            source_name: None,
            source_uuid: None,
            data_message: Some(DataMessage {
                message: Some(text.into()),
                timestamp: Some(1700000000000),
                group_info: None,
                attachments: None,
            }),
            story_message: None,
            timestamp: None,
        }
    }

    fn make_group_envelope(sender: &str, text: &str, group_id: &str) -> Envelope {
        Envelope {
            source: None,
            source_number: Some(sender.into()),
            source_name: None,
            source_uuid: None,
            data_message: Some(DataMessage {
                message: Some(text.into()),
                timestamp: Some(1700000000000),
                group_info: Some(GroupInfo {
                    group_id: Some(group_id.into()),
                }),
                attachments: None,
            }),
            story_message: None,
            timestamp: None,
        }
    }

    #[test]
    fn process_envelope_dm_allowed_by_uuid() {
        // Test that UUID matching works when sender has no phone number
        let perms = SignalPermissions {
            group_filter: None,
            dm_allowed_users: vec!["uuid-1234".to_string()],
            group_allowed_users: vec![],
        };
        let adapter = test_adapter_with_permissions(perms);
        let envelope = Envelope {
            source: None,
            source_number: None,
            source_name: None,
            source_uuid: Some("uuid-1234".into()),
            data_message: Some(DataMessage {
                message: Some("hello".into()),
                timestamp: Some(1700000000000),
                group_info: None,
                attachments: None,
            }),
            story_message: None,
            timestamp: None,
        };
        let result = adapter.process_envelope(&envelope);
        assert!(
            result.is_some(),
            "DM should be allowed when sender UUID matches"
        );
    }

    #[test]
    fn process_envelope_group_message_rejected_when_sender_not_allowed() {
        // Group message from sender not in group_allowed_users should be rejected
        let perms = SignalPermissions {
            group_filter: Some(vec!["grp123".to_string()]),
            dm_allowed_users: vec!["+9999999999".to_string()], // Different from sender
            group_allowed_users: vec!["+9999999999".to_string()],
        };
        let adapter = test_adapter_with_permissions(perms);
        let envelope = make_group_envelope("+1111111111", "hi group", "grp123");
        let result = adapter.process_envelope(&envelope);
        assert!(
            result.is_none(),
            "Group message should be rejected when sender not in allowed users"
        );
    }
}

#[cfg(test)]
mod rpc_error_tests {
    use super::*;

    #[test]
    fn rpc_params_build_direct_message() {
        let adapter = test_adapter_with_permissions(SignalPermissions::default());
        let params = adapter.build_rpc_params(
            &RecipientTarget::Direct("+1234567890".to_string()),
            Some("test message"),
            None,
        );
        // Verify recipient is an array (signal-cli requirement)
        assert!(params.get("recipient").is_some());
        assert!(params["recipient"].is_array());
    }

    #[test]
    fn rpc_params_build_group_message() {
        let adapter = test_adapter_with_permissions(SignalPermissions::default());
        let params = adapter.build_rpc_params(
            &RecipientTarget::Group("base64groupid".to_string()),
            Some("group message"),
            None,
        );
        // Verify groupId is used instead of recipient
        assert!(params.get("groupId").is_some());
        assert!(params.get("recipient").is_none());
    }

    #[test]
    fn rpc_params_with_attachments() {
        let adapter = test_adapter_with_permissions(SignalPermissions::default());
        let attachments = vec!["/tmp/file1.jpg".to_string(), "/tmp/file2.png".to_string()];
        let params = adapter.build_rpc_params(
            &RecipientTarget::Direct("+1234567890".to_string()),
            Some("check this out"),
            Some(&attachments),
        );
        assert!(params.get("attachments").is_some());
        let attachments_arr = params["attachments"].as_array().unwrap();
        assert_eq!(attachments_arr.len(), 2);
    }

    fn test_adapter_with_permissions(perms: SignalPermissions) -> SignalAdapter {
        SignalAdapter {
            runtime_key: "signal".into(),
            http_url: "http://127.0.0.1:8686".into(),
            account: "+0000000000".into(),
            ignore_stories: true,
            permissions: Arc::new(ArcSwap::from_pointee(perms)),
            client: reqwest::Client::new(),
            tmp_dir: PathBuf::from("/tmp/spacebot-test"),
            reply_targets: Arc::new(RwLock::new(LruCache::new(
                NonZeroUsize::new(10_000).expect("10_000 is non-zero"),
            ))),
            typing_tasks: Arc::new(RwLock::new(HashMap::new())),
            shutdown_tx: Arc::new(RwLock::new(None)),
        }
    }
}
