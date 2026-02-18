//! Discord messaging adapter using serenity.

use crate::config::DiscordPermissions;
use crate::messaging::traits::{HistoryMessage, InboundStream, Messaging};
use crate::{InboundMessage, MessageContent, OutboundResponse, StatusUpdate};

use anyhow::Context as _;
use arc_swap::ArcSwap;
use async_trait::async_trait;
use serenity::all::{
    ChannelId, ChannelType, Context, CreateAttachment, CreateMessage, CreateThread, EditMessage,
    EventHandler, GatewayIntents, GetMessages, Http, Message, MessageId, ReactionType, Ready,
    ShardManager, User, UserId,
};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{RwLock, mpsc};

/// Discord adapter state.
pub struct DiscordAdapter {
    token: String,
    permissions: Arc<ArcSwap<DiscordPermissions>>,
    http: Arc<RwLock<Option<Arc<Http>>>>,
    bot_user_id: Arc<RwLock<Option<UserId>>>,
    /// Maps InboundMessage.id to the Discord MessageId being edited during streaming.
    active_messages: Arc<RwLock<HashMap<String, serenity::all::MessageId>>>,
    /// Typing handles per message. Typing stops when the handle is dropped.
    typing_tasks: Arc<RwLock<HashMap<String, serenity::http::Typing>>>,
    shard_manager: Arc<RwLock<Option<Arc<ShardManager>>>>,
}

impl DiscordAdapter {
    pub fn new(token: impl Into<String>, permissions: Arc<ArcSwap<DiscordPermissions>>) -> Self {
        Self {
            token: token.into(),
            permissions,
            http: Arc::new(RwLock::new(None)),
            bot_user_id: Arc::new(RwLock::new(None)),
            active_messages: Arc::new(RwLock::new(HashMap::new())),
            typing_tasks: Arc::new(RwLock::new(HashMap::new())),
            shard_manager: Arc::new(RwLock::new(None)),
        }
    }

    async fn get_http(&self) -> anyhow::Result<Arc<Http>> {
        self.http
            .read()
            .await
            .clone()
            .context("discord not connected")
    }

    fn extract_channel_id(&self, message: &InboundMessage) -> anyhow::Result<ChannelId> {
        let id = message
            .metadata
            .get("discord_channel_id")
            .and_then(|v| v.as_u64())
            .context("missing discord_channel_id in metadata")?;
        Ok(ChannelId::new(id))
    }

    async fn stop_typing(&self, message_id: &str) {
        // Typing stops when the handle is dropped
        self.typing_tasks.write().await.remove(message_id);
    }
}

impl Messaging for DiscordAdapter {
    fn name(&self) -> &str {
        "discord"
    }

    async fn start(&self) -> crate::Result<InboundStream> {
        let (inbound_tx, inbound_rx) = mpsc::channel(256);

        let handler = Handler {
            inbound_tx,
            permissions: self.permissions.clone(),
            http_slot: self.http.clone(),
            bot_user_id_slot: self.bot_user_id.clone(),
        };

        let intents = GatewayIntents::GUILD_MESSAGES
            | GatewayIntents::DIRECT_MESSAGES
            | GatewayIntents::MESSAGE_CONTENT
            | GatewayIntents::GUILDS;

        let mut client = serenity::Client::builder(&self.token, intents)
            .event_handler(handler)
            .await
            .context("failed to build discord client")?;

        *self.http.write().await = Some(client.http.clone());
        *self.shard_manager.write().await = Some(client.shard_manager.clone());

        tokio::spawn(async move {
            if let Err(error) = client.start().await {
                tracing::error!(%error, "discord gateway error");
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
        let http = self.get_http().await?;
        let channel_id = self.extract_channel_id(message)?;

        match response {
            OutboundResponse::Text(text) => {
                self.stop_typing(&message.id).await;

                for chunk in split_message(&text, 2000) {
                    channel_id
                        .say(&*http, &chunk)
                        .await
                        .context("failed to send discord message")?;
                }
            }
            OutboundResponse::ThreadReply { thread_name, text } => {
                self.stop_typing(&message.id).await;

                // Try to create a public thread from the source message.
                // Requires the "Create Public Threads" bot permission.
                let message_id = message
                    .metadata
                    .get("discord_message_id")
                    .and_then(|v| v.as_u64())
                    .map(MessageId::new);

                let thread_result = match message_id {
                    Some(source_message_id) => {
                        let builder =
                            CreateThread::new(&thread_name).kind(ChannelType::PublicThread);
                        channel_id
                            .create_thread_from_message(&*http, source_message_id, builder)
                            .await
                    }
                    None => {
                        let builder =
                            CreateThread::new(&thread_name).kind(ChannelType::PublicThread);
                        channel_id.create_thread(&*http, builder).await
                    }
                };

                match thread_result {
                    Ok(thread) => {
                        for chunk in split_message(&text, 2000) {
                            thread
                                .id
                                .say(&*http, &chunk)
                                .await
                                .context("failed to send message in new thread")?;
                        }
                    }
                    Err(error) => {
                        // Fall back to a regular message if thread creation fails
                        // (e.g. missing permissions, DM context)
                        tracing::warn!(
                            %error,
                            thread_name = %thread_name,
                            "failed to create thread, falling back to regular message"
                        );
                        for chunk in split_message(&text, 2000) {
                            channel_id
                                .say(&*http, &chunk)
                                .await
                                .context("failed to send discord message")?;
                        }
                    }
                }
            }
            OutboundResponse::File {
                filename,
                data,
                mime_type: _,
                caption,
            } => {
                self.stop_typing(&message.id).await;

                let attachment = CreateAttachment::bytes(data, &filename);
                let mut builder = CreateMessage::new().add_file(attachment);
                if let Some(caption_text) = caption {
                    builder = builder.content(caption_text);
                }

                channel_id
                    .send_message(&*http, builder)
                    .await
                    .context("failed to send file attachment")?;
            }
            OutboundResponse::Reaction(emoji) => {
                let message_id = message
                    .metadata
                    .get("discord_message_id")
                    .and_then(|v| v.as_u64())
                    .context("missing discord_message_id for reaction")?;

                channel_id
                    .create_reaction(
                        &*http,
                        MessageId::new(message_id),
                        ReactionType::Unicode(emoji),
                    )
                    .await
                    .context("failed to add reaction")?;
            }
            OutboundResponse::StreamStart => {
                self.stop_typing(&message.id).await;

                let placeholder = channel_id
                    .say(&*http, "\u{200B}")
                    .await
                    .context("failed to send stream placeholder")?;

                self.active_messages
                    .write()
                    .await
                    .insert(message.id.clone(), placeholder.id);
            }
            OutboundResponse::StreamChunk(text) => {
                let active = self.active_messages.read().await;
                if let Some(&message_id) = active.get(&message.id) {
                    let display_text = if text.len() > 2000 {
                        let end = text.floor_char_boundary(1997);
                        format!("{}...", &text[..end])
                    } else {
                        text
                    };
                    let builder = EditMessage::new().content(display_text);
                    if let Err(error) = channel_id.edit_message(&*http, message_id, builder).await {
                        tracing::warn!(%error, "failed to edit streaming message");
                    }
                }
            }
            OutboundResponse::StreamEnd => {
                self.active_messages.write().await.remove(&message.id);
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
                let http = self.get_http().await?;
                let channel_id = self.extract_channel_id(message)?;

                let typing = channel_id.start_typing(&http);
                self.typing_tasks
                    .write()
                    .await
                    .insert(message.id.clone(), typing);
            }
            _ => {
                self.stop_typing(&message.id).await;
            }
        }

        Ok(())
    }

    async fn broadcast(&self, target: &str, response: OutboundResponse) -> crate::Result<()> {
        let http = self.get_http().await?;

        let channel_id = ChannelId::new(
            target
                .parse::<u64>()
                .context("invalid discord channel id for broadcast target")?,
        );

        if let OutboundResponse::Text(text) = response {
            for chunk in split_message(&text, 2000) {
                channel_id
                    .say(&*http, &chunk)
                    .await
                    .context("failed to broadcast discord message")?;
            }
        }

        Ok(())
    }

    async fn fetch_history(
        &self,
        message: &InboundMessage,
        limit: usize,
    ) -> crate::Result<Vec<HistoryMessage>> {
        let http = self.get_http().await?;
        let channel_id = self.extract_channel_id(message)?;

        let message_id = message
            .metadata
            .get("discord_message_id")
            .and_then(|v| v.as_u64())
            .context("missing discord_message_id in metadata")?;

        // Fetch messages before the triggering message (capped at 100 per Discord API)
        let capped_limit = limit.min(100) as u8;
        let builder = GetMessages::new()
            .before(MessageId::new(message_id))
            .limit(capped_limit);

        let messages = channel_id
            .messages(&*http, builder)
            .await
            .context("failed to fetch discord message history")?;

        let bot_user_id = self.bot_user_id.read().await;

        // Messages come back newest-first from Discord, reverse to chronological
        let history: Vec<HistoryMessage> = messages
            .iter()
            .rev()
            .map(|message| {
                let is_bot = bot_user_id
                    .map(|bot_id| message.author.id == bot_id)
                    .unwrap_or(false);

                let resolved_content = resolve_mentions(&message.content, &message.mentions);

                let display_name = message
                    .author
                    .global_name
                    .as_deref()
                    .unwrap_or(&message.author.name);

                // Include reply-to attribution if this message is a reply
                let author = if let Some(referenced) = &message.referenced_message {
                    let reply_author = referenced
                        .author
                        .global_name
                        .as_deref()
                        .unwrap_or(&referenced.author.name);
                    format!("{display_name} (replying to {reply_author})")
                } else {
                    display_name.to_string()
                };

                HistoryMessage {
                    author,
                    content: resolved_content,
                    is_bot,
                }
            })
            .collect();

        tracing::info!(
            count = history.len(),
            channel_id = %channel_id,
            "fetched discord message history"
        );

        Ok(history)
    }

    async fn health_check(&self) -> crate::Result<()> {
        let http = self.get_http().await?;
        http.get_current_user()
            .await
            .context("discord health check failed")?;
        Ok(())
    }

    async fn shutdown(&self) -> crate::Result<()> {
        self.typing_tasks.write().await.clear();

        if let Some(shard_manager) = self.shard_manager.read().await.as_ref() {
            shard_manager.shutdown_all().await;
        }

        tracing::info!("discord adapter shut down");
        Ok(())
    }
}

// -- Serenity EventHandler --

struct Handler {
    inbound_tx: mpsc::Sender<InboundMessage>,
    permissions: Arc<ArcSwap<DiscordPermissions>>,
    http_slot: Arc<RwLock<Option<Arc<Http>>>>,
    bot_user_id_slot: Arc<RwLock<Option<UserId>>>,
}

#[async_trait]
impl EventHandler for Handler {
    async fn ready(&self, ctx: Context, ready: Ready) {
        tracing::info!(bot_name = %ready.user.name, "discord connected");

        *self.http_slot.write().await = Some(ctx.http.clone());
        *self.bot_user_id_slot.write().await = Some(ready.user.id);
        tracing::info!(guild_count = ready.guilds.len(), "discord guilds available");
    }

    async fn message(&self, ctx: Context, message: Message) {
        // Always ignore our own messages to prevent self-response loops
        let bot_user_id = self.bot_user_id_slot.read().await;
        if bot_user_id.is_some_and(|id| message.author.id == id) {
            return;
        }
        drop(bot_user_id);

        // Load a snapshot of the current permissions (hot-reloadable)
        let permissions = self.permissions.load();

        // Filter other bots unless explicitly allowed
        if message.author.bot && !permissions.allow_bot_messages {
            return;
        }

        // DM filter: if no guild_id, it's a DM — only allow listed users
        if message.guild_id.is_none() {
            if permissions.dm_allowed_users.is_empty()
                || !permissions
                    .dm_allowed_users
                    .contains(&message.author.id.get())
            {
                return;
            }
        }

        if let Some(filter) = &permissions.guild_filter {
            if let Some(guild_id) = message.guild_id {
                if !filter.contains(&guild_id.get()) {
                    return;
                }
            }
        }

        let conversation_id = build_conversation_id(&message);
        let content = extract_content(&message);
        let metadata = build_metadata(&ctx, &message).await;

        // Channel filter: allow if the channel ID or its parent (for threads) is in the allowlist
        if let Some(guild_id) = message.guild_id {
            if let Some(allowed_channels) = permissions.channel_filter.get(&guild_id.get()) {
                if !allowed_channels.is_empty() {
                    let parent_channel_id = metadata
                        .get("discord_parent_channel_id")
                        .and_then(|v| v.as_u64());

                    let direct_match = allowed_channels.contains(&message.channel_id.get());
                    let parent_match =
                        parent_channel_id.is_some_and(|pid| allowed_channels.contains(&pid));

                    if !direct_match && !parent_match {
                        return;
                    }
                }
            }
        }

        let inbound = InboundMessage {
            id: message.id.to_string(),
            source: "discord".into(),
            conversation_id,
            sender_id: message.author.id.to_string(),
            agent_id: None,
            content,
            timestamp: *message.timestamp,
            metadata,
        };

        if let Err(error) = self.inbound_tx.send(inbound).await {
            tracing::warn!(
                %error,
                "failed to send inbound message from Discord (receiver dropped)"
            );
        }
    }
}

// -- Helper functions --

fn build_conversation_id(message: &Message) -> String {
    match message.guild_id {
        Some(guild_id) => format!("discord:{}:{}", guild_id, message.channel_id),
        None => format!("discord:dm:{}", message.author.id),
    }
}

fn extract_content(message: &Message) -> MessageContent {
    let resolved_content = resolve_mentions(&message.content, &message.mentions);

    if message.attachments.is_empty() {
        MessageContent::Text(resolved_content)
    } else {
        let attachments = message
            .attachments
            .iter()
            .map(|attachment| crate::Attachment {
                filename: attachment.filename.clone(),
                mime_type: attachment.content_type.clone().unwrap_or_default(),
                url: attachment.url.clone(),
                size_bytes: Some(attachment.size as u64),
            })
            .collect();

        MessageContent::Media {
            text: if resolved_content.is_empty() {
                None
            } else {
                Some(resolved_content)
            },
            attachments,
        }
    }
}

/// Replace raw Discord mention syntax (`<@ID>` and `<@!ID>`) with readable display names.
/// Serenity provides resolved `User` objects in `message.mentions` for every mention in the text.
fn resolve_mentions(content: &str, mentions: &[User]) -> String {
    let mut resolved = content.to_string();
    for user in mentions {
        let display_name = user.global_name.as_deref().unwrap_or(&user.name);

        let mention_pattern = format!("<@{}>", user.id);
        resolved = resolved.replace(&mention_pattern, &format!("@{display_name}"));

        // Legacy nickname mention format
        let nick_pattern = format!("<@!{}>", user.id);
        resolved = resolved.replace(&nick_pattern, &format!("@{display_name}"));
    }
    resolved
}

async fn build_metadata(ctx: &Context, message: &Message) -> HashMap<String, serde_json::Value> {
    let mut metadata = HashMap::new();
    metadata.insert("discord_channel_id".into(), message.channel_id.get().into());
    metadata.insert("discord_message_id".into(), message.id.get().into());
    metadata.insert(
        "discord_author_name".into(),
        message.author.name.clone().into(),
    );

    // Display name: member nickname > global display name > username
    let display_name = if let Some(member) = &message.member {
        member.nick.clone().unwrap_or_else(|| {
            message
                .author
                .global_name
                .clone()
                .unwrap_or_else(|| message.author.name.clone())
        })
    } else {
        message
            .author
            .global_name
            .clone()
            .unwrap_or_else(|| message.author.name.clone())
    };
    metadata.insert("sender_display_name".into(), display_name.into());
    metadata.insert("sender_id".into(), message.author.id.get().into());
    if message.author.bot {
        metadata.insert("sender_is_bot".into(), true.into());
    }

    if let Some(guild_id) = message.guild_id {
        metadata.insert("discord_guild_id".into(), guild_id.get().into());

        // Try to get guild name
        if let Ok(guild) = guild_id.to_partial_guild(&ctx.http).await {
            metadata.insert("discord_guild_name".into(), guild.name.into());
        }
    }

    // Try to get channel name and detect threads
    if let Ok(channel) = message.channel_id.to_channel(&ctx.http).await {
        if let Some(guild_channel) = channel.guild() {
            metadata.insert(
                "discord_channel_name".into(),
                guild_channel.name.clone().into(),
            );

            // Threads have a parent_id pointing to the text channel they were created in
            if guild_channel.thread_metadata.is_some() {
                metadata.insert("discord_is_thread".into(), true.into());
                if let Some(parent_id) = guild_channel.parent_id {
                    metadata.insert("discord_parent_channel_id".into(), parent_id.get().into());
                }
            }
        }
    }

    // Reply-to context: resolve the referenced message's author and content
    if let Some(referenced) = &message.referenced_message {
        let reply_author = referenced
            .author
            .global_name
            .as_deref()
            .unwrap_or(&referenced.author.name);
        metadata.insert("reply_to_author".into(), reply_author.into());
        metadata.insert("reply_to_is_bot".into(), referenced.author.bot.into());

        let reply_content = resolve_mentions(&referenced.content, &referenced.mentions);
        // Truncate to avoid bloating context with long quoted messages
        let truncated = if reply_content.len() > 200 {
            format!("{}...", &reply_content[..200])
        } else {
            reply_content
        };
        metadata.insert("reply_to_content".into(), truncated.into());
    }

    metadata
}

/// Split a message into chunks that fit within Discord's 2000 char limit.
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
