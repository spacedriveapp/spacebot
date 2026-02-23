//! Telegram messaging adapter using teloxide.

use crate::config::TelegramPermissions;
use crate::messaging::traits::{InboundStream, Messaging};
use crate::{Attachment, InboundMessage, MessageContent, OutboundResponse, StatusUpdate};

use anyhow::Context as _;
use arc_swap::ArcSwap;
use regex::Regex;
use teloxide::payloads::setters::*;
use teloxide::requests::{Request, Requester};
use teloxide::types::{
    ChatAction, ChatId, FileId, InputFile, InputPollOption, MediaKind, MessageId, MessageKind,
    ParseMode, ReactionType, ReplyParameters, UpdateKind, UserId,
};
use teloxide::{ApiError, Bot, RequestError};

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, LazyLock};
use std::time::Instant;
use tokio::sync::{RwLock, mpsc};
use tokio::task::JoinHandle;

/// Maximum number of rejected DM users to remember.
const REJECTED_USERS_CAPACITY: usize = 50;

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

/// Smaller source-chunk target for markdown that expands heavily when HTML-escaped.
const FORMATTED_SPLIT_LENGTH: usize = MAX_MESSAGE_LENGTH / 2;

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
            // Track users whose DMs were rejected so we can nudge them when they're allowed.
            let mut rejected_users: VecDeque<(ChatId, i64)> = VecDeque::new();
            // Snapshot the current allow list so we can detect changes.
            let mut last_allowed: Vec<i64> = permissions.load().dm_allowed_users.clone();

            loop {
                tokio::select! {
                    _ = shutdown_rx.recv() => {
                        tracing::info!("telegram polling loop shutting down");
                        break;
                    }
                    result = bot.get_updates().offset(offset).timeout(10).send() => {
                        let updates = match result {
                            Ok(updates) => updates,
                            Err(error) => {
                                tracing::error!(%error, "telegram getUpdates failed");
                                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                                continue;
                            }
                        };

                        // Check if the allow list changed and nudge newly-allowed users.
                        let current_permissions = permissions.load();
                        if current_permissions.dm_allowed_users != last_allowed {
                            let newly_allowed: Vec<i64> = current_permissions.dm_allowed_users.iter()
                                .filter(|id| !last_allowed.contains(id))
                                .copied()
                                .collect();

                            if !newly_allowed.is_empty() {
                                // Notify rejected users who are now allowed.
                                let mut remaining = VecDeque::new();
                                for (chat_id, user_id) in rejected_users.drain(..) {
                                    if newly_allowed.contains(&user_id) {
                                        tracing::info!(
                                            user_id,
                                            "notifying previously rejected user they are now allowed"
                                        );
                                        let _ = bot.send_message(
                                            chat_id,
                                            "You've been added to the allow list — send me a message!",
                                        ).send().await;
                                    } else {
                                        remaining.push_back((chat_id, user_id));
                                    }
                                }
                                rejected_users = remaining;
                            }

                            last_allowed = current_permissions.dm_allowed_users.clone();
                        }

                        for update in updates {
                            offset = update.id.as_offset();

                            let message = match &update.kind {
                                UpdateKind::Message(message) => message,
                                _ => continue,
                            };

                            let bot_id = *bot_user_id.read().await;

                            // Skip our own messages
                            if let Some(from) = &message.from
                                && bot_id.is_some_and(|id| from.id == id) {
                                    continue;
                                }

                            let permissions = permissions.load();

                            let chat_id = message.chat.id.0;
                            let is_private = message.chat.is_private();

                            // DM filter: in private chats, check dm_allowed_users
                            if is_private {
                                if let Some(from) = &message.from
                                    && !permissions.dm_allowed_users.is_empty()
                                        && !permissions
                                            .dm_allowed_users
                                            .contains(&(from.id.0 as i64))
                                    {
                                        // Remember this user so we can nudge them if they're added later.
                                        let entry = (message.chat.id, from.id.0 as i64);
                                        if !rejected_users.iter().any(|(_, uid)| *uid == entry.1) {
                                            if rejected_users.len() >= REJECTED_USERS_CAPACITY {
                                                rejected_users.pop_front();
                                            }
                                            rejected_users.push_back(entry);
                                        }
                                        continue;
                                    }
                            } else if let Some(filter) = &permissions.chat_filter {
                                // Chat filter: if configured, only allow listed group/channel chats
                                if !filter.contains(&chat_id) {
                                    tracing::debug!(
                                        chat_id,
                                        ?filter,
                                        "telegram message rejected by chat filter"
                                    );
                                    continue;
                                }
                            }

                            // Extract text content
                            let text = extract_text(message);
                            if text.is_none() && !has_attachments(message) {
                                continue;
                            }

                            let content = build_content(&bot, message, &text).await;
                            let conversation_id = format!("telegram:{chat_id}");
                            let sender_id = message
                                .from
                                .as_ref()
                                .map(|u| u.id.0.to_string())
                                .unwrap_or_default();

                            let (metadata, formatted_author) = build_metadata(
                                message,
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
                                formatted_author,
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
                send_formatted(&self.bot, chat_id, &text, None).await?;
            }
            OutboundResponse::RichMessage { text, poll, .. } => {
                self.stop_typing(&message.conversation_id).await;
                send_formatted(&self.bot, chat_id, &text, None).await?;

                if let Some(poll_data) = poll {
                    send_poll(&self.bot, chat_id, &poll_data).await?;
                }
            }
            OutboundResponse::ThreadReply {
                thread_name: _,
                text,
            } => {
                self.stop_typing(&message.conversation_id).await;

                // Telegram doesn't have named threads. Reply to the source message instead.
                let reply_to = self.extract_message_id(message).ok();
                send_formatted(&self.bot, chat_id, &text, reply_to).await?;
            }
            OutboundResponse::File {
                filename,
                data,
                mime_type,
                caption,
            } => {
                self.stop_typing(&message.conversation_id).await;

                // Use send_audio for audio files so Telegram renders an inline player.
                // Fall back to send_document for everything else.
                if mime_type.starts_with("audio/") {
                    let input_file = InputFile::memory(data.clone()).file_name(filename.clone());
                    let sent = if let Some(ref caption_text) = caption {
                        let html_caption = markdown_to_telegram_html(caption_text);
                        self.bot
                            .send_audio(chat_id, input_file)
                            .caption(&html_caption)
                            .parse_mode(ParseMode::Html)
                            .send()
                            .await
                    } else {
                        self.bot.send_audio(chat_id, input_file).send().await
                    };

                    if let Err(error) = sent {
                        if should_retry_plain_caption(&error) {
                            tracing::debug!(
                                %error,
                                "HTML caption parse failed, retrying telegram audio with plain caption"
                            );
                            let fallback_file = InputFile::memory(data).file_name(filename);
                            let mut request = self.bot.send_audio(chat_id, fallback_file);
                            if let Some(caption_text) = caption {
                                request = request.caption(caption_text);
                            }
                            request
                                .send()
                                .await
                                .context("failed to send telegram audio")?;
                        } else {
                            return Err(error)
                                .context("failed to send telegram audio with HTML caption")?;
                        }
                    }
                } else {
                    let input_file = InputFile::memory(data.clone()).file_name(filename.clone());
                    let sent = if let Some(ref caption_text) = caption {
                        let html_caption = markdown_to_telegram_html(caption_text);
                        self.bot
                            .send_document(chat_id, input_file)
                            .caption(&html_caption)
                            .parse_mode(ParseMode::Html)
                            .send()
                            .await
                    } else {
                        self.bot.send_document(chat_id, input_file).send().await
                    };

                    if let Err(error) = sent {
                        if should_retry_plain_caption(&error) {
                            tracing::debug!(
                                %error,
                                "HTML caption parse failed, retrying telegram file with plain caption"
                            );
                            let fallback_file = InputFile::memory(data).file_name(filename);
                            let mut request = self.bot.send_document(chat_id, fallback_file);
                            if let Some(caption_text) = caption {
                                request = request.caption(caption_text);
                            }
                            request
                                .send()
                                .await
                                .context("failed to send telegram file")?;
                        } else {
                            return Err(error)
                                .context("failed to send telegram file with HTML caption")?;
                        }
                    }
                }
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
                    if stream.last_edit.elapsed() < STREAM_EDIT_INTERVAL {
                        return Ok(());
                    }

                    let display_text = if text.len() > MAX_MESSAGE_LENGTH {
                        let end = text.floor_char_boundary(MAX_MESSAGE_LENGTH - 3);
                        format!("{}...", &text[..end])
                    } else {
                        text
                    };

                    let html = markdown_to_telegram_html(&display_text);
                    if let Err(html_error) = self
                        .bot
                        .edit_message_text(stream.chat_id, stream.message_id, &html)
                        .parse_mode(ParseMode::Html)
                        .send()
                        .await
                    {
                        tracing::debug!(%html_error, "HTML edit failed, retrying as plain text");
                        if let Err(error) = self
                            .bot
                            .edit_message_text(stream.chat_id, stream.message_id, &display_text)
                            .send()
                            .await
                        {
                            tracing::debug!(%error, "failed to edit streaming message");
                        }
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
            // Slack-specific variants — graceful fallbacks for Telegram
            OutboundResponse::RemoveReaction(_) => {} // no-op
            OutboundResponse::Ephemeral { text, .. } => {
                // Telegram has no ephemeral messages — send as regular text
                send_formatted(&self.bot, chat_id, &text, None).await?;
            }
            OutboundResponse::ScheduledMessage { text, .. } => {
                // Telegram has no scheduled messages — send immediately
                send_formatted(&self.bot, chat_id, &text, None).await?;
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

    async fn broadcast(&self, target: &str, response: OutboundResponse) -> crate::Result<()> {
        let chat_id = ChatId(
            target
                .parse::<i64>()
                .context("invalid telegram chat id for broadcast target")?,
        );

        if let OutboundResponse::Text(text) = response {
            send_formatted(&self.bot, chat_id, &text, None).await?;
        } else if let OutboundResponse::RichMessage { text, poll, .. } = response {
            send_formatted(&self.bot, chat_id, &text, None).await?;

            if let Some(poll_data) = poll {
                send_poll(&self.bot, chat_id, &poll_data).await?;
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
///
/// Resolves Telegram file IDs to download URLs via the Bot API.
async fn build_content(
    bot: &Bot,
    message: &teloxide::types::Message,
    text: &Option<String>,
) -> MessageContent {
    let attachments = extract_attachments(message);

    if attachments.is_empty() {
        return MessageContent::Text(text.clone().unwrap_or_default());
    }

    let mut resolved = Vec::with_capacity(attachments.len());
    for mut attachment in attachments {
        match resolve_file_url(bot, &attachment.url).await {
            Ok(url) => attachment.url = url,
            Err(error) => {
                tracing::warn!(
                    file_id = %attachment.url,
                    %error,
                    "failed to resolve telegram file URL, skipping attachment"
                );
                continue;
            }
        }
        resolved.push(attachment);
    }

    if resolved.is_empty() {
        MessageContent::Text(text.clone().unwrap_or_default())
    } else {
        MessageContent::Media {
            text: text.clone(),
            attachments: resolved,
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

/// Resolve a Telegram file ID to a download URL via the Bot API.
///
/// Telegram doesn't provide direct URLs for file attachments. Instead you get a file ID
/// that must be resolved through `getFile` to obtain the actual download path.
async fn resolve_file_url(bot: &Bot, file_id: &str) -> anyhow::Result<String> {
    let file = bot
        .get_file(FileId(file_id.to_string()))
        .send()
        .await
        .context("getFile API call failed")?;

    let mut url = bot.api_url();
    {
        let mut segments = url
            .path_segments_mut()
            .map_err(|_| anyhow::anyhow!("cannot-be-a-base URL"))?;
        segments.push("file");
        segments.push(&format!("bot{}", bot.token()));
        segments.push(&file.path);
    }

    Ok(url.to_string())
}

/// Build platform-specific metadata for a Telegram message.
fn build_metadata(
    message: &teloxide::types::Message,
    bot_username: &Option<String>,
) -> (HashMap<String, serde_json::Value>, Option<String>) {
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

    let formatted_author = if let Some(from) = &message.from {
        metadata.insert(
            "telegram_user_id".into(),
            serde_json::Value::Number(from.id.0.into()),
        );

        let display_name = build_display_name(from);
        metadata.insert("display_name".into(), display_name.clone().into());
        metadata.insert("sender_display_name".into(), display_name.clone().into());

        let author = if let Some(username) = &from.username {
            metadata.insert("telegram_username".into(), username.clone().into());
            metadata.insert(
                "telegram_user_mention".into(),
                serde_json::Value::String(format!("@{}", username)),
            );
            format!("{} (@{})", display_name, username)
        } else {
            display_name
        };
        Some(author)
    } else {
        None
    };

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

    (metadata, formatted_author)
}

/// Build a display name from a Telegram user, preferring full name.
fn build_display_name(user: &teloxide::types::User) -> String {
    let first = &user.first_name;
    match &user.last_name {
        Some(last) => format!("{first} {last}"),
        None => first.clone(),
    }
}

/// Send a native Telegram poll.
///
/// Telegram limits: max 12 answer options, question max 300 chars, each option
/// max 100 chars. `open_period` only supports 5–600 seconds so we only set it
/// when `duration_hours` converts to ≤600s; otherwise the poll stays open
/// indefinitely (until manually stopped via the Telegram client).
async fn send_poll(bot: &Bot, chat_id: ChatId, poll: &crate::Poll) -> anyhow::Result<()> {
    let question = if poll.question.len() > 300 {
        format!(
            "{}…",
            &poll.question[..poll.question.floor_char_boundary(299)]
        )
    } else {
        poll.question.clone()
    };

    let options: Vec<InputPollOption> = poll
        .answers
        .iter()
        .take(12)
        .map(|answer| {
            let text = if answer.len() > 100 {
                format!("{}…", &answer[..answer.floor_char_boundary(99)])
            } else {
                answer.clone()
            };
            InputPollOption::new(text)
        })
        .collect();

    if options.len() < 2 {
        anyhow::bail!("telegram polls require at least 2 answer options");
    }

    let mut request = bot
        .send_poll(chat_id, question, options)
        .is_anonymous(false);

    // Telegram's open_period only supports 5–600 seconds. Apply it when the
    // requested duration fits; otherwise leave unset so the poll stays open
    // indefinitely.
    let duration_secs = poll.duration_hours.saturating_mul(3600);
    if (5..=600).contains(&duration_secs) {
        request = request.open_period(duration_secs as u16);
    }

    if poll.allow_multiselect {
        request = request.allows_multiple_answers(true);
    }

    request
        .send()
        .await
        .context("failed to send telegram poll")?;

    Ok(())
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

/// Return true when Telegram rejected rich text entities and a plain-caption retry is safe.
fn should_retry_plain_caption(error: &RequestError) -> bool {
    matches!(error, RequestError::Api(ApiError::CantParseEntities(_)))
}

// -- Markdown-to-Telegram-HTML formatting --

static BOLD_ITALIC_PATTERN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\*\*\*(.+?)\*\*\*").expect("hardcoded regex"));
static BOLD_PATTERN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\*\*(.+?)\*\*").expect("hardcoded regex"));
static BOLD_UNDERSCORE_PATTERN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"__(.+?)__").expect("hardcoded regex"));
static ITALIC_PATTERN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\*(.+?)\*").expect("hardcoded regex"));
static ITALIC_UNDERSCORE_PATTERN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"_(.+?)_").expect("hardcoded regex"));
static STRIKETHROUGH_PATTERN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"~~(.+?)~~").expect("hardcoded regex"));
static LINK_PATTERN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\[([^\]]+)\]\(([^)]+)\)").expect("hardcoded regex"));

/// Escape characters that have special meaning in Telegram's HTML parse mode.
fn escape_html(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Strip HTML tags and unescape entities, producing plain text for fallback.
fn strip_html_tags(html: &str) -> String {
    static TAG_PATTERN: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"<[^>]+>").expect("hardcoded regex"));
    TAG_PATTERN
        .replace_all(html, "")
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
}

/// Convert markdown to Telegram-compatible HTML.
///
/// Handles fenced code blocks, inline code, bold, italic, strikethrough,
/// links, headers (rendered as bold), and blockquotes.
fn markdown_to_telegram_html(markdown: &str) -> String {
    let mut result = String::with_capacity(markdown.len());
    let mut in_code_block = false;
    let mut code_language = String::new();
    let mut code_lines: Vec<&str> = Vec::new();
    let mut blockquote_lines: Vec<String> = Vec::new();

    for line in markdown.lines() {
        if let Some(rest) = line.strip_prefix("```") {
            flush_blockquote(&mut result, &mut blockquote_lines);

            if in_code_block {
                let content = escape_html(&code_lines.join("\n"));
                if code_language.is_empty() {
                    result.push_str("<pre>");
                    result.push_str(&content);
                    result.push_str("</pre>\n");
                } else {
                    result.push_str("<pre><code class=\"language-");
                    result.push_str(&code_language);
                    result.push_str("\">");
                    result.push_str(&content);
                    result.push_str("</code></pre>\n");
                }
                in_code_block = false;
                code_language.clear();
                code_lines.clear();
            } else {
                in_code_block = true;
                code_language = rest.trim().to_string();
            }
            continue;
        }

        if in_code_block {
            code_lines.push(line);
            continue;
        }

        if let Some(quote_text) = line.strip_prefix("> ") {
            blockquote_lines.push(format_inline(quote_text));
            continue;
        } else if line == ">" {
            blockquote_lines.push(String::new());
            continue;
        }

        flush_blockquote(&mut result, &mut blockquote_lines);

        if let Some(header_text) = line
            .strip_prefix("### ")
            .or_else(|| line.strip_prefix("## "))
            .or_else(|| line.strip_prefix("# "))
        {
            result.push_str("<b>");
            result.push_str(&format_inline(header_text));
            result.push_str("</b>\n");
            continue;
        }

        result.push_str(&format_inline(line));
        result.push('\n');
    }

    if in_code_block {
        result.push_str("<pre>");
        result.push_str(&escape_html(&code_lines.join("\n")));
        result.push_str("</pre>\n");
    }

    flush_blockquote(&mut result, &mut blockquote_lines);

    while result.ends_with('\n') {
        result.pop();
    }

    result
}

/// Append buffered blockquote lines to the result and clear the buffer.
fn flush_blockquote(result: &mut String, lines: &mut Vec<String>) {
    if lines.is_empty() {
        return;
    }
    result.push_str("<blockquote>");
    result.push_str(&lines.join("\n"));
    result.push_str("</blockquote>\n");
    lines.clear();
}

/// Convert inline markdown elements to HTML within a single line.
///
/// Splits on backticks to isolate inline code spans, then converts bold,
/// italic, strikethrough and links in the remaining text. Content inside
/// backticks is HTML-escaped but not processed for markdown.
fn format_inline(line: &str) -> String {
    let segments: Vec<&str> = line.split('`').collect();
    let mut result = String::new();

    for (index, segment) in segments.iter().enumerate() {
        if index % 2 == 1 && index < segments.len() - 1 {
            result.push_str("<code>");
            result.push_str(&escape_html(segment));
            result.push_str("</code>");
        } else if index % 2 == 0 {
            result.push_str(&format_markdown_spans(&escape_html(segment)));
        } else {
            // Unmatched trailing backtick — treat as literal
            result.push('`');
            result.push_str(&format_markdown_spans(&escape_html(segment)));
        }
    }

    result
}

/// Replace markdown span markers with HTML tags in already-escaped text.
///
/// Bold-italic (`***`) is processed first, then bold (`**`), then italic
/// (`*`) so longer patterns are consumed before shorter ones.
fn format_markdown_spans(text: &str) -> String {
    let text = BOLD_ITALIC_PATTERN.replace_all(text, "<b><i>$1</i></b>");
    let text = BOLD_PATTERN.replace_all(&text, "<b>$1</b>");
    let text = BOLD_UNDERSCORE_PATTERN.replace_all(&text, "<b>$1</b>");
    let text = ITALIC_PATTERN.replace_all(&text, "<i>$1</i>");
    let text = ITALIC_UNDERSCORE_PATTERN.replace_all(&text, "<i>$1</i>");
    let text = STRIKETHROUGH_PATTERN.replace_all(&text, "<s>$1</s>");
    let text = LINK_PATTERN.replace_all(&text, r#"<a href="$2">$1</a>"#);
    text.into_owned()
}

/// Send a plain text Telegram message for formatting fallback paths.
async fn send_plain_text(
    bot: &Bot,
    chat_id: ChatId,
    text: &str,
    reply_to: Option<MessageId>,
) -> anyhow::Result<()> {
    let mut request = bot.send_message(chat_id, text);
    if let Some(reply_id) = reply_to {
        request = request.reply_parameters(ReplyParameters::new(reply_id));
    }
    request
        .send()
        .await
        .context("failed to send telegram message")?;
    Ok(())
}

/// Send a message with Telegram HTML formatting, splitting at the message
/// length limit. Falls back to plain text if the API rejects the HTML.
async fn send_formatted(
    bot: &Bot,
    chat_id: ChatId,
    text: &str,
    reply_to: Option<MessageId>,
) -> anyhow::Result<()> {
    let mut pending_chunks: VecDeque<String> =
        VecDeque::from(split_message(text, MAX_MESSAGE_LENGTH));
    while let Some(markdown_chunk) = pending_chunks.pop_front() {
        let html_chunk = markdown_to_telegram_html(&markdown_chunk);

        if html_chunk.len() > MAX_MESSAGE_LENGTH {
            let smaller_chunks = split_message(&markdown_chunk, FORMATTED_SPLIT_LENGTH);
            if smaller_chunks.len() > 1 {
                for chunk in smaller_chunks.into_iter().rev() {
                    pending_chunks.push_front(chunk);
                }
                continue;
            }

            let plain_chunk = strip_html_tags(&html_chunk);
            send_plain_text(bot, chat_id, &plain_chunk, reply_to).await?;
            continue;
        }

        let mut request = bot
            .send_message(chat_id, &html_chunk)
            .parse_mode(ParseMode::Html);
        if let Some(reply_id) = reply_to {
            request = request.reply_parameters(ReplyParameters::new(reply_id));
        }
        if let Err(error) = request.send().await {
            tracing::debug!(%error, "HTML send failed, retrying as plain text");
            let plain_chunk = strip_html_tags(&html_chunk);
            send_plain_text(bot, chat_id, &plain_chunk, reply_to).await?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bold() {
        assert_eq!(
            markdown_to_telegram_html("**bold text**"),
            "<b>bold text</b>"
        );
    }

    #[test]
    fn italic() {
        assert_eq!(
            markdown_to_telegram_html("*italic text*"),
            "<i>italic text</i>"
        );
    }

    #[test]
    fn bold_with_underscores() {
        assert_eq!(
            markdown_to_telegram_html("__bold text__"),
            "<b>bold text</b>"
        );
    }

    #[test]
    fn italic_with_underscores() {
        assert_eq!(
            markdown_to_telegram_html("_italic text_"),
            "<i>italic text</i>"
        );
    }

    #[test]
    fn bold_and_italic_nested() {
        assert_eq!(
            markdown_to_telegram_html("***both***"),
            "<b><i>both</i></b>"
        );
    }

    #[test]
    fn inline_code() {
        assert_eq!(
            markdown_to_telegram_html("use `println!` here"),
            "use <code>println!</code> here"
        );
    }

    #[test]
    fn code_block_with_language() {
        let input = "```rust\nfn main() {}\n```";
        let expected = "<pre><code class=\"language-rust\">fn main() {}</code></pre>";
        assert_eq!(markdown_to_telegram_html(input), expected);
    }

    #[test]
    fn code_block_without_language() {
        let input = "```\nhello world\n```";
        let expected = "<pre>hello world</pre>";
        assert_eq!(markdown_to_telegram_html(input), expected);
    }

    #[test]
    fn code_block_escapes_html() {
        let input = "```\n<script>alert(1)</script>\n```";
        let expected = "<pre>&lt;script&gt;alert(1)&lt;/script&gt;</pre>";
        assert_eq!(markdown_to_telegram_html(input), expected);
    }

    #[test]
    fn link() {
        assert_eq!(
            markdown_to_telegram_html("[click](https://example.com)"),
            r#"<a href="https://example.com">click</a>"#
        );
    }

    #[test]
    fn strikethrough() {
        assert_eq!(markdown_to_telegram_html("~~deleted~~"), "<s>deleted</s>");
    }

    #[test]
    fn headers_render_as_bold() {
        assert_eq!(markdown_to_telegram_html("# Title"), "<b>Title</b>");
        assert_eq!(markdown_to_telegram_html("## Sub"), "<b>Sub</b>");
        assert_eq!(markdown_to_telegram_html("### Section"), "<b>Section</b>");
    }

    #[test]
    fn blockquote() {
        assert_eq!(
            markdown_to_telegram_html("> quoted text"),
            "<blockquote>quoted text</blockquote>"
        );
    }

    #[test]
    fn multiline_blockquote() {
        let input = "> line one\n> line two";
        let expected = "<blockquote>line one\nline two</blockquote>";
        assert_eq!(markdown_to_telegram_html(input), expected);
    }

    #[test]
    fn html_entities_escaped_in_text() {
        assert_eq!(
            markdown_to_telegram_html("x < y & a > b"),
            "x &lt; y &amp; a &gt; b"
        );
    }

    #[test]
    fn inline_code_escapes_html() {
        assert_eq!(
            markdown_to_telegram_html("`<b>not bold</b>`"),
            "<code>&lt;b&gt;not bold&lt;/b&gt;</code>"
        );
    }

    #[test]
    fn mixed_formatting() {
        let input = "Hello **world**, this is *important* and `code`";
        let expected = "Hello <b>world</b>, this is <i>important</i> and <code>code</code>";
        assert_eq!(markdown_to_telegram_html(input), expected);
    }

    #[test]
    fn plain_text_unchanged() {
        assert_eq!(
            markdown_to_telegram_html("just plain text"),
            "just plain text"
        );
    }

    #[test]
    fn unclosed_code_block_handled() {
        let input = "```python\nprint('hi')";
        let expected = "<pre>print('hi')</pre>";
        assert_eq!(markdown_to_telegram_html(input), expected);
    }

    #[test]
    fn strip_html_tags_and_unescape() {
        assert_eq!(
            strip_html_tags("<b>bold</b> &amp; <i>italic</i>"),
            "bold & italic"
        );
    }

    #[test]
    fn list_items_pass_through() {
        let input = "- item one\n- item two\n- item three";
        let expected = "- item one\n- item two\n- item three";
        assert_eq!(markdown_to_telegram_html(input), expected);
    }

    #[test]
    fn retries_plain_caption_only_for_parse_entity_errors() {
        let parse_error = RequestError::Api(ApiError::CantParseEntities(
            "Bad Request: can't parse entities".into(),
        ));
        let non_parse_error = RequestError::Api(ApiError::BotBlocked);

        assert!(should_retry_plain_caption(&parse_error));
        assert!(!should_retry_plain_caption(&non_parse_error));
    }
}
