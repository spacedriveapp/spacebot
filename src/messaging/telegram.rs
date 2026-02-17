//! Telegram messaging adapter using teloxide.

use crate::config::TelegramPermissions;
use crate::messaging::traits::{InboundStream, Messaging};
use crate::{Attachment, InboundMessage, MessageContent, OutboundResponse, StatusUpdate};

use anyhow::Context as _;
use arc_swap::ArcSwap;
use teloxide::Bot;
use teloxide::payloads::setters::*;
use teloxide::requests::{Request, Requester};
use teloxide::types::{
    ChatAction, ChatId, InputFile, MediaKind, MessageId, MessageKind, ReactionType,
    ReplyParameters, UpdateKind, UserId,
};

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::{RwLock, mpsc};
use tokio::task::JoinHandle;

/// Telegram adapter state.
pub struct TelegramAdapter {
    permissions: Arc<ArcSwap<TelegramPermissions>>,
    bot: Bot,
    bot_user_id: Arc<RwLock<Option<UserId>>>,
    bot_username: Arc<RwLock<Option<String>>>,
    /// Maps conversation_id to the message_id being edited during streaming.
    active_messages: Arc<RwLock<HashMap<String, ActiveStream>>>,
    /// Repeating typing indicator tasks per conversation_id.
    typing_tasks: Arc<RwLock<HashMap<String, JoinHandle<()>>>>,
    /// Shutdown signal for the polling loop.
    shutdown_tx: Arc<RwLock<Option<mpsc::Sender<()>>>>,
}

/// Tracks an in-progress streaming message edit.
struct ActiveStream {
    chat_id: ChatId,
    message_id: MessageId,
    last_edit: Instant,
}

/// Telegram's per-message character limit.
const MAX_MESSAGE_LENGTH: usize = 4096;

/// Minimum interval between streaming edits to avoid rate limits.
const STREAM_EDIT_INTERVAL: std::time::Duration = std::time::Duration::from_millis(1000);

impl TelegramAdapter {
    pub fn new(token: impl Into<String>, permissions: Arc<ArcSwap<TelegramPermissions>>) -> Self {
        let token = token.into();
        let bot = Bot::new(&token);
        Self {
            permissions,
            bot,
            bot_user_id: Arc::new(RwLock::new(None)),
            bot_username: Arc::new(RwLock::new(None)),
            active_messages: Arc::new(RwLock::new(HashMap::new())),
            typing_tasks: Arc::new(RwLock::new(HashMap::new())),
            shutdown_tx: Arc::new(RwLock::new(None)),
        }
    }

    fn extract_chat_id(&self, message: &InboundMessage) -> anyhow::Result<ChatId> {
        let id = message
            .metadata
            .get("telegram_chat_id")
            .and_then(|v| v.as_i64())
            .context("missing telegram_chat_id in metadata")?;
        Ok(ChatId(id))
    }

    fn extract_message_id(&self, message: &InboundMessage) -> anyhow::Result<MessageId> {
        let id = message
            .metadata
            .get("telegram_message_id")
            .and_then(|v| v.as_i64())
            .map(|v| v as i32)
            .context("missing telegram_message_id in metadata")?;
        Ok(MessageId(id))
    }

    async fn stop_typing(&self, conversation_id: &str) {
        if let Some(handle) = self.typing_tasks.write().await.remove(conversation_id) {
            handle.abort();
        }
    }
}

impl Messaging for TelegramAdapter {
    fn name(&self) -> &str {
        "telegram"
    }

    async fn start(&self) -> crate::Result<InboundStream> {
        let (inbound_tx, inbound_rx) = mpsc::channel(256);
        let (shutdown_tx, mut shutdown_rx) = mpsc::channel::<()>(1);

        *self.shutdown_tx.write().await = Some(shutdown_tx);

        // Resolve bot identity
        let me = self
            .bot
            .get_me()
            .send()
            .await
            .context("failed to call getMe on Telegram")?;
        *self.bot_user_id.write().await = Some(me.id);
        *self.bot_username.write().await = me.username.clone();
        tracing::info!(
            bot_name = %me.first_name,
            bot_username = ?me.username,
            "telegram connected"
        );

        let bot = self.bot.clone();
        let permissions = self.permissions.clone();
        let bot_user_id = self.bot_user_id.clone();
        let bot_username = self.bot_username.clone();

        tokio::spawn(async move {
            let mut offset = 0i32;

            loop {
                tokio::select! {
                    _ = shutdown_rx.recv() => {
                        tracing::info!("telegram polling loop shutting down");
                        break;
                    }
                    result = bot.get_updates().offset(offset).timeout(30).send() => {
                        let updates = match result {
                            Ok(updates) => updates,
                            Err(error) => {
                                tracing::error!(%error, "telegram getUpdates failed");
                                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                                continue;
                            }
                        };

                        for update in updates {
                            offset = update.id.as_offset() as i32;

                            let message = match &update.kind {
                                UpdateKind::Message(message) => message,
                                _ => continue,
                            };

                            let bot_id = *bot_user_id.read().await;

                            // Skip our own messages
                            if let Some(from) = &message.from {
                                if bot_id.is_some_and(|id| from.id == id) {
                                    continue;
                                }
                            }

                            let permissions = permissions.load();

                            let chat_id = message.chat.id.0;
                            let is_private = message.chat.is_private();

                            // DM filter: in private chats, check dm_allowed_users
                            if is_private {
                                if let Some(from) = &message.from {
                                    if !permissions.dm_allowed_users.is_empty()
                                        && !permissions
                                            .dm_allowed_users
                                            .contains(&(from.id.0 as i64))
                                    {
                                        continue;
                                    }
                                }
                            }

                            // Chat filter: if configured, only allow listed chats
                            if let Some(filter) = &permissions.chat_filter {
                                if !filter.contains(&chat_id) {
                                    continue;
                                }
                            }

                            // Extract text content
                            let text = extract_text(&message);
                            if text.is_none() && !has_attachments(&message) {
                                continue;
                            }

                            let content = build_content(&message, &text);
                            let conversation_id = format!("telegram:{chat_id}");
                            let sender_id = message
                                .from
                                .as_ref()
                                .map(|u| u.id.0.to_string())
                                .unwrap_or_default();

                            let metadata = build_metadata(
                                &message,
                                &*bot_username.read().await,
                            );

                            let inbound = InboundMessage {
                                id: message.id.0.to_string(),
                                source: "telegram".into(),
                                conversation_id,
                                sender_id,
                                agent_id: None,
                                content,
                                timestamp: message.date,
                                metadata,
                            };

                            if let Err(error) = inbound_tx.send(inbound).await {
                                tracing::warn!(
                                    %error,
                                    "failed to send inbound message from Telegram (receiver dropped)"
                                );
                                return;
                            }
                        }
                    }
                }
            }
        });

        let stream = tokio_stream::wrappers::ReceiverStream::new(inbound_rx);
        Ok(Box::pin(stream))
    }

    async fn respond(
        &self,
        message: &InboundMessage,
        response: OutboundResponse,
    ) -> crate::Result<()> {
        let chat_id = self.extract_chat_id(message)?;

        match response {
            OutboundResponse::Text(text) => {
                self.stop_typing(&message.conversation_id).await;

                for chunk in split_message(&text, MAX_MESSAGE_LENGTH) {
                    self.bot
                        .send_message(chat_id, &chunk)
                        .send()
                        .await
                        .context("failed to send telegram message")?;
                }
            }
            OutboundResponse::ThreadReply {
                thread_name: _,
                text,
            } => {
                self.stop_typing(&message.conversation_id).await;

                // Telegram doesn't have named threads. Reply to the source message instead.
                let reply_to = self.extract_message_id(message).ok();

                for chunk in split_message(&text, MAX_MESSAGE_LENGTH) {
                    let mut request = self.bot.send_message(chat_id, &chunk);
                    if let Some(reply_id) = reply_to {
                        request = request.reply_parameters(ReplyParameters::new(reply_id));
                    }
                    request
                        .send()
                        .await
                        .context("failed to send telegram thread reply")?;
                }
            }
            OutboundResponse::File {
                filename,
                data,
                mime_type: _,
                caption,
            } => {
                self.stop_typing(&message.conversation_id).await;

                let input_file = InputFile::memory(data).file_name(filename);
                let mut request = self.bot.send_document(chat_id, input_file);
                if let Some(caption_text) = caption {
                    request = request.caption(caption_text);
                }
                request
                    .send()
                    .await
                    .context("failed to send telegram file")?;
            }
            OutboundResponse::Reaction(emoji) => {
                let message_id = self.extract_message_id(message)?;

                let reaction = ReactionType::Emoji {
                    emoji: emoji.clone(),
                };
                if let Err(error) = self
                    .bot
                    .set_message_reaction(chat_id, message_id)
                    .reaction(vec![reaction])
                    .send()
                    .await
                {
                    // Telegram only supports a limited set of reaction emojis per chat.
                    // Log and continue rather than failing the response.
                    tracing::debug!(
                        %error,
                        emoji = %emoji,
                        "failed to set telegram reaction (emoji may not be available in this chat)"
                    );
                }
            }
            OutboundResponse::StreamStart => {
                self.stop_typing(&message.conversation_id).await;

                let placeholder = self
                    .bot
                    .send_message(chat_id, "...")
                    .send()
                    .await
                    .context("failed to send stream placeholder")?;

                self.active_messages.write().await.insert(
                    message.conversation_id.clone(),
                    ActiveStream {
                        chat_id,
                        message_id: placeholder.id,
                        last_edit: Instant::now(),
                    },
                );
            }
            OutboundResponse::StreamChunk(text) => {
                let mut active = self.active_messages.write().await;
                if let Some(stream) = active.get_mut(&message.conversation_id) {
                    // Rate-limit edits to avoid Telegram API throttling
                    if stream.last_edit.elapsed() < STREAM_EDIT_INTERVAL {
                        return Ok(());
                    }

                    let display_text = if text.len() > MAX_MESSAGE_LENGTH {
                        let end = text.floor_char_boundary(MAX_MESSAGE_LENGTH - 3);
                        format!("{}...", &text[..end])
                    } else {
                        text
                    };

                    if let Err(error) = self
                        .bot
                        .edit_message_text(stream.chat_id, stream.message_id, display_text)
                        .send()
                        .await
                    {
                        tracing::debug!(%error, "failed to edit streaming message");
                    }
                    stream.last_edit = Instant::now();
                }
            }
            OutboundResponse::StreamEnd => {
                self.active_messages
                    .write()
                    .await
                    .remove(&message.conversation_id);
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
                let chat_id = self.extract_chat_id(message)?;
                let bot = self.bot.clone();
                let conversation_id = message.conversation_id.clone();

                // Telegram typing indicators expire after 5 seconds.
                // Send one immediately, then repeat every 4 seconds.
                let handle = tokio::spawn(async move {
                    loop {
                        if let Err(error) = bot
                            .send_chat_action(chat_id, ChatAction::Typing)
                            .send()
                            .await
                        {
                            tracing::debug!(%error, "failed to send typing indicator");
                            break;
                        }
                        tokio::time::sleep(std::time::Duration::from_secs(4)).await;
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
        self.bot
            .get_me()
            .send()
            .await
            .context("telegram health check failed")?;
        Ok(())
    }

    async fn shutdown(&self) -> crate::Result<()> {
        // Cancel all typing indicator tasks
        let mut tasks = self.typing_tasks.write().await;
        for (_, handle) in tasks.drain() {
            handle.abort();
        }

        // Signal the polling loop to stop
        if let Some(tx) = self.shutdown_tx.read().await.as_ref() {
            tx.send(()).await.ok();
        }

        tracing::info!("telegram adapter shut down");
        Ok(())
    }
}

// -- Helper functions --

/// Extract text content from a Telegram message.
fn extract_text(message: &teloxide::types::Message) -> Option<String> {
    match &message.kind {
        MessageKind::Common(common) => match &common.media_kind {
            MediaKind::Text(text) => Some(text.text.clone()),
            MediaKind::Photo(photo) => photo.caption.clone(),
            MediaKind::Document(doc) => doc.caption.clone(),
            MediaKind::Video(video) => video.caption.clone(),
            MediaKind::Voice(voice) => voice.caption.clone(),
            MediaKind::Audio(audio) => audio.caption.clone(),
            _ => None,
        },
        _ => None,
    }
}

/// Check if a message contains file attachments.
fn has_attachments(message: &teloxide::types::Message) -> bool {
    match &message.kind {
        MessageKind::Common(common) => matches!(
            &common.media_kind,
            MediaKind::Photo(_)
                | MediaKind::Document(_)
                | MediaKind::Video(_)
                | MediaKind::Voice(_)
                | MediaKind::Audio(_)
        ),
        _ => false,
    }
}

/// Build `MessageContent` from a Telegram message.
fn build_content(message: &teloxide::types::Message, text: &Option<String>) -> MessageContent {
    let attachments = extract_attachments(message);

    if attachments.is_empty() {
        MessageContent::Text(text.clone().unwrap_or_default())
    } else {
        MessageContent::Media {
            text: text.clone(),
            attachments,
        }
    }
}

/// Extract file attachment metadata from a Telegram message.
fn extract_attachments(message: &teloxide::types::Message) -> Vec<Attachment> {
    let mut attachments = Vec::new();

    let MessageKind::Common(common) = &message.kind else {
        return attachments;
    };

    match &common.media_kind {
        MediaKind::Photo(photo) => {
            // Use the largest photo size
            if let Some(largest) = photo.photo.last() {
                attachments.push(Attachment {
                    filename: format!("photo_{}.jpg", largest.file.unique_id),
                    mime_type: "image/jpeg".into(),
                    url: largest.file.id.to_string(),
                    size_bytes: Some(largest.file.size as u64),
                });
            }
        }
        MediaKind::Document(doc) => {
            attachments.push(Attachment {
                filename: doc
                    .document
                    .file_name
                    .clone()
                    .unwrap_or_else(|| "document".into()),
                mime_type: doc
                    .document
                    .mime_type
                    .as_ref()
                    .map(|m| m.to_string())
                    .unwrap_or_else(|| "application/octet-stream".into()),
                url: doc.document.file.id.to_string(),
                size_bytes: Some(doc.document.file.size as u64),
            });
        }
        MediaKind::Video(video) => {
            attachments.push(Attachment {
                filename: video
                    .video
                    .file_name
                    .clone()
                    .unwrap_or_else(|| "video.mp4".into()),
                mime_type: video
                    .video
                    .mime_type
                    .as_ref()
                    .map(|m| m.to_string())
                    .unwrap_or_else(|| "video/mp4".into()),
                url: video.video.file.id.to_string(),
                size_bytes: Some(video.video.file.size as u64),
            });
        }
        MediaKind::Voice(voice) => {
            attachments.push(Attachment {
                filename: "voice.ogg".into(),
                mime_type: voice
                    .voice
                    .mime_type
                    .as_ref()
                    .map(|m| m.to_string())
                    .unwrap_or_else(|| "audio/ogg".into()),
                url: voice.voice.file.id.to_string(),
                size_bytes: Some(voice.voice.file.size as u64),
            });
        }
        MediaKind::Audio(audio) => {
            attachments.push(Attachment {
                filename: audio
                    .audio
                    .file_name
                    .clone()
                    .unwrap_or_else(|| "audio".into()),
                mime_type: audio
                    .audio
                    .mime_type
                    .as_ref()
                    .map(|m| m.to_string())
                    .unwrap_or_else(|| "audio/mpeg".into()),
                url: audio.audio.file.id.to_string(),
                size_bytes: Some(audio.audio.file.size as u64),
            });
        }
        _ => {}
    }

    attachments
}

/// Build platform-specific metadata for a Telegram message.
fn build_metadata(
    message: &teloxide::types::Message,
    bot_username: &Option<String>,
) -> HashMap<String, serde_json::Value> {
    let mut metadata = HashMap::new();

    metadata.insert(
        "telegram_chat_id".into(),
        serde_json::Value::Number(message.chat.id.0.into()),
    );
    metadata.insert(
        "telegram_message_id".into(),
        serde_json::Value::Number(message.id.0.into()),
    );

    let chat_type = if message.chat.is_private() {
        "private"
    } else if message.chat.is_group() {
        "group"
    } else if message.chat.is_supergroup() {
        "supergroup"
    } else if message.chat.is_channel() {
        "channel"
    } else {
        "unknown"
    };
    metadata.insert("telegram_chat_type".into(), chat_type.into());

    if let Some(title) = &message.chat.title() {
        metadata.insert("telegram_chat_title".into(), (*title).into());
    }

    if let Some(from) = &message.from {
        metadata.insert(
            "telegram_user_id".into(),
            serde_json::Value::Number(from.id.0.into()),
        );

        let display_name = build_display_name(from);
        metadata.insert("display_name".into(), display_name.into());

        if let Some(username) = &from.username {
            metadata.insert("telegram_username".into(), username.clone().into());
        }
    }

    if let Some(bot_username) = bot_username {
        metadata.insert("telegram_bot_username".into(), bot_username.clone().into());
    }

    // Reply-to context for threading
    if let Some(reply) = message.reply_to_message() {
        metadata.insert(
            "reply_to_message_id".into(),
            serde_json::Value::Number(reply.id.0.into()),
        );
        if let Some(text) = extract_text(reply) {
            let truncated = if text.len() > 200 {
                format!("{}...", &text[..text.floor_char_boundary(197)])
            } else {
                text
            };
            metadata.insert("reply_to_text".into(), truncated.into());
        }
        if let Some(from) = &reply.from {
            metadata.insert("reply_to_author".into(), build_display_name(from).into());
        }
    }

    metadata
}

/// Build a display name from a Telegram user, preferring full name.
fn build_display_name(user: &teloxide::types::User) -> String {
    let first = &user.first_name;
    match &user.last_name {
        Some(last) => format!("{first} {last}"),
        None => first.clone(),
    }
}

/// Split a message into chunks that fit within Telegram's character limit.
/// Tries to split at newlines, then spaces, then hard-cuts.
fn split_message(text: &str, max_len: usize) -> Vec<String> {
    if text.len() <= max_len {
        return vec![text.to_string()];
    }

    let mut chunks = Vec::new();
    let mut remaining = text;

    while !remaining.is_empty() {
        if remaining.len() <= max_len {
            chunks.push(remaining.to_string());
            break;
        }

        let split_at = remaining[..max_len]
            .rfind('\n')
            .or_else(|| remaining[..max_len].rfind(' '))
            .unwrap_or(max_len);

        chunks.push(remaining[..split_at].to_string());
        remaining = remaining[split_at..].trim_start();
    }

    chunks
}
