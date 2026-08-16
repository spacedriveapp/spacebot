//! Discord messaging adapter using serenity.

use crate::config::DiscordPermissions;
use crate::messaging::apply_runtime_adapter_to_conversation_id;
use crate::messaging::traits::{HistoryMessage, InboundStream, Messaging};
use crate::{InboundMessage, MessageContent, OutboundResponse, StatusUpdate};

use anyhow::Context as _;
use arc_swap::ArcSwap;
use async_trait::async_trait;
use serenity::all::{
    ButtonStyle, ChannelId, ChannelType, Command as ApplicationCommand, CommandDataOptionValue,
    CommandInteraction, CommandOptionType, Context, CreateActionRow, CreateAttachment,
    CreateButton, CreateCommand, CreateCommandOption, CreateEmbed, CreateEmbedAuthor,
    CreateEmbedFooter, CreateInteractionResponse, CreateInteractionResponseMessage, CreateMessage,
    CreatePoll, CreatePollAnswer, CreateSelectMenu, CreateSelectMenuKind, CreateSelectMenuOption,
    CreateThread, EditInteractionResponse, EditMessage, EventHandler, GatewayIntents, GetMessages,
    GuildId, Http, Interaction, Message, MessageId, ReactionType, Ready, ShardManager, Timestamp,
    User, UserId,
};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::{RwLock, mpsc};

/// Discord interaction tokens are valid for 15 minutes; stop trying to
/// resolve deferred responses a minute early.
const INTERACTION_TOKEN_TTL: std::time::Duration = std::time::Duration::from_secs(14 * 60);

/// A deferred interaction awaiting its real response.
struct InteractionToken {
    token: String,
    created: Instant,
}

/// Discord adapter state.
pub struct DiscordAdapter {
    runtime_key: String,
    token: String,
    permissions: Arc<ArcSwap<DiscordPermissions>>,
    http: Arc<RwLock<Option<Arc<Http>>>>,
    bot_user_id: Arc<RwLock<Option<UserId>>>,
    /// Maps InboundMessage.id to the Discord MessageId being edited during streaming.
    active_messages: Arc<RwLock<HashMap<String, serenity::all::MessageId>>>,
    /// Typing handles per message. Typing stops when the handle is dropped.
    typing_tasks: Arc<RwLock<HashMap<String, serenity::http::Typing>>>,
    shard_manager: Arc<RwLock<Option<Arc<ShardManager>>>>,
    /// Deferred slash-command interactions keyed by InboundMessage.id, so
    /// replies can resolve the deferral within the token window.
    interaction_tokens: Arc<RwLock<HashMap<String, InteractionToken>>>,
    /// Guards against concurrent application-command syncs when `ready`
    /// re-fires on reconnect.
    command_sync_active: Arc<std::sync::atomic::AtomicBool>,
}

impl DiscordAdapter {
    pub fn new(
        runtime_key: impl Into<String>,
        token: impl Into<String>,
        permissions: Arc<ArcSwap<DiscordPermissions>>,
    ) -> Self {
        Self {
            runtime_key: runtime_key.into(),
            token: token.into(),
            permissions,
            http: Arc::new(RwLock::new(None)),
            bot_user_id: Arc::new(RwLock::new(None)),
            active_messages: Arc::new(RwLock::new(HashMap::new())),
            typing_tasks: Arc::new(RwLock::new(HashMap::new())),
            shard_manager: Arc::new(RwLock::new(None)),
            interaction_tokens: Arc::new(RwLock::new(HashMap::new())),
            command_sync_active: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }
    }

    /// Take the deferred-interaction token for a message, if still within
    /// the token window. Expired entries are pruned on the way.
    async fn take_interaction_token(&self, message_id: &str) -> Option<String> {
        let mut tokens = self.interaction_tokens.write().await;
        tokens.retain(|_, entry| entry.created.elapsed() < INTERACTION_TOKEN_TTL);
        tokens.remove(message_id).map(|entry| entry.token)
    }

    /// Clear a deferred interaction whose real response is going out through
    /// a surface that can't resolve it (rich messages, streaming). Deleting
    /// the deferral removes the dangling "thinking" state.
    async fn clear_deferred_interaction(&self, http: &Arc<Http>, message_id: &str) {
        if let Some(token) = self.take_interaction_token(message_id).await {
            let http = http.clone();
            tokio::spawn(async move {
                if let Err(error) = http.delete_original_interaction_response(&token).await {
                    tracing::debug!(%error, "failed to delete deferred interaction response");
                }
            });
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

    fn channel_key(message: &InboundMessage) -> String {
        message
            .metadata
            .get("discord_channel_id")
            .and_then(|v| v.as_u64())
            .map(|id| id.to_string())
            .unwrap_or_else(|| message.id.clone())
    }

    async fn stop_typing(&self, message: &InboundMessage) {
        // Keyed by channel ID so stale message IDs can't leave handles orphaned
        self.typing_tasks
            .write()
            .await
            .remove(&Self::channel_key(message));
    }

    fn extract_reply_message_id(message: &InboundMessage) -> Option<MessageId> {
        message
            .metadata
            .get(crate::metadata_keys::REPLY_TO_MESSAGE_ID)
            .and_then(|value| match value {
                serde_json::Value::String(s) => s.parse::<u64>().ok(),
                serde_json::Value::Number(n) => n.as_u64(),
                _ => None,
            })
            .map(MessageId::new)
    }
}

impl Messaging for DiscordAdapter {
    fn name(&self) -> &str {
        &self.runtime_key
    }

    async fn start(&self) -> crate::Result<InboundStream> {
        let (inbound_tx, inbound_rx) = mpsc::channel(256);

        let handler = Handler {
            inbound_tx,
            runtime_key: self.runtime_key.clone(),
            permissions: self.permissions.clone(),
            http_slot: self.http.clone(),
            bot_user_id_slot: self.bot_user_id.clone(),
            interaction_tokens: self.interaction_tokens.clone(),
            command_sync_active: self.command_sync_active.clone(),
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
                self.stop_typing(message).await;
                let reply_to = Self::extract_reply_message_id(message);
                let deferred = self.take_interaction_token(&message.id).await;

                for (index, chunk) in split_message(&text, 2000).into_iter().enumerate() {
                    // The first chunk resolves a deferred slash-command
                    // interaction in place; overflow chunks follow as
                    // ordinary messages.
                    if index == 0
                        && let Some(token) = deferred.as_deref()
                    {
                        let edit = EditInteractionResponse::new().content(chunk.as_str());
                        match http
                            .edit_original_interaction_response(token, &edit, Vec::new())
                            .await
                        {
                            Ok(_) => continue,
                            Err(error) => {
                                tracing::warn!(
                                    %error,
                                    "failed to resolve deferred interaction, falling back to channel send"
                                );
                            }
                        }
                    }
                    let mut builder = CreateMessage::new().content(chunk);
                    if index == 0
                        && let Some(reply_message_id) = reply_to
                    {
                        builder = builder.reference_message((channel_id, reply_message_id));
                    }
                    channel_id
                        .send_message(&*http, builder)
                        .await
                        .context("failed to send discord message")?;
                }
            }
            OutboundResponse::RichMessage {
                text,
                cards,
                interactive_elements,
                poll,
                ..
            } => {
                self.stop_typing(message).await;
                self.clear_deferred_interaction(&http, &message.id).await;
                let reply_to = Self::extract_reply_message_id(message);
                let parts =
                    prepare_rich_message_parts(text, &cards, &interactive_elements, poll.as_ref());
                if parts.dropped_invalid_poll {
                    tracing::warn!(
                        "dropping invalid discord poll payload while sending rich message"
                    );
                }

                let chunks = split_message(&parts.text, 2000);
                for (i, chunk) in chunks.iter().enumerate() {
                    let is_last = i == chunks.len() - 1;
                    let mut msg = CreateMessage::new();
                    if !chunk.is_empty() {
                        msg = msg.content(chunk);
                    }

                    // Attach rich content only to the final chunk
                    if is_last {
                        let embeds: Vec<_> = parts.cards.iter().map(build_embed).collect();
                        if !embeds.is_empty() {
                            msg = msg.embeds(embeds);
                        }

                        let components: Vec<_> = parts
                            .interactive_elements
                            .iter()
                            .map(build_action_row)
                            .collect();
                        if !components.is_empty() {
                            msg = msg.components(components);
                        }

                        if let Some(poll_data) = parts.poll.as_ref().and_then(build_poll) {
                            msg = msg.poll(poll_data);
                        }
                    }

                    if i == 0
                        && let Some(reply_message_id) = reply_to
                    {
                        msg = msg.reference_message((channel_id, reply_message_id));
                    }

                    channel_id
                        .send_message(&*http, msg)
                        .await
                        .context("failed to send discord rich message")?;
                }
            }
            OutboundResponse::ThreadReply { thread_name, text } => {
                self.stop_typing(message).await;

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
                self.stop_typing(message).await;
                let reply_to = Self::extract_reply_message_id(message);

                let attachment = CreateAttachment::bytes(data, &filename);
                let mut builder = CreateMessage::new().add_file(attachment);
                if let Some(caption_text) = caption {
                    builder = builder.content(caption_text);
                }
                if let Some(reply_message_id) = reply_to {
                    builder = builder.reference_message((channel_id, reply_message_id));
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
                self.stop_typing(message).await;
                self.clear_deferred_interaction(&http, &message.id).await;

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
            // Slack-specific variants — graceful fallbacks for Discord
            OutboundResponse::RemoveReaction(_) => {} // no-op
            OutboundResponse::Ephemeral { text, .. } => {
                // A deferred slash command resolves ephemerally in place;
                // outside an interaction Discord has no ephemeral surface,
                // so degrade to a regular message.
                if let Some(token) = self.take_interaction_token(&message.id).await {
                    let edit = EditInteractionResponse::new().content(&text);
                    if http
                        .edit_original_interaction_response(&token, &edit, Vec::new())
                        .await
                        .is_ok()
                    {
                        return Ok(());
                    }
                }
                channel_id
                    .say(&*http, &text)
                    .await
                    .context("failed to send ephemeral fallback on discord")?;
            }
            OutboundResponse::ScheduledMessage { text, .. } => {
                // Discord has no native scheduled messages — send immediately
                if let Ok(channel_id) = self.extract_channel_id(message) {
                    let http = self.get_http().await?;
                    channel_id
                        .say(&*http, &text)
                        .await
                        .context("failed to send scheduled message fallback on discord")?;
                }
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
                    .insert(Self::channel_key(message), typing);
            }
            _ => {
                self.stop_typing(message).await;
            }
        }

        Ok(())
    }

    async fn broadcast(&self, target: &str, response: OutboundResponse) -> crate::Result<()> {
        let http = self.get_http().await?;

        // Support "dm:{user_id}" targets for opening DM channels
        let channel_id = if let Some(user_id_str) = target.strip_prefix("dm:") {
            let user_id = UserId::new(
                user_id_str
                    .parse::<u64>()
                    .context("invalid discord user id for DM broadcast target")?,
            );
            user_id
                .create_dm_channel(&*http)
                .await
                .context("failed to open DM channel")?
                .id
        } else {
            ChannelId::new(
                target
                    .parse::<u64>()
                    .context("invalid discord channel id for broadcast target")?,
            )
        };

        if let OutboundResponse::Text(text) = response {
            for chunk in split_message(&text, 2000) {
                channel_id
                    .say(&*http, &chunk)
                    .await
                    .context("failed to broadcast discord message")?;
            }
        } else if let OutboundResponse::RichMessage {
            text,
            cards,
            interactive_elements,
            poll,
            ..
        } = response
        {
            let parts =
                prepare_rich_message_parts(text, &cards, &interactive_elements, poll.as_ref());
            if parts.dropped_invalid_poll {
                tracing::warn!(
                    "dropping invalid discord poll payload while broadcasting rich message"
                );
            }

            let chunks = split_message(&parts.text, 2000);
            for (i, chunk) in chunks.iter().enumerate() {
                let is_last = i == chunks.len() - 1;
                let mut msg = CreateMessage::new();
                if !chunk.is_empty() {
                    msg = msg.content(chunk);
                }

                // Attach rich content only to the final chunk
                if is_last {
                    let embeds: Vec<_> = parts.cards.iter().map(build_embed).collect();
                    if !embeds.is_empty() {
                        msg = msg.embeds(embeds);
                    }

                    let components: Vec<_> = parts
                        .interactive_elements
                        .iter()
                        .map(build_action_row)
                        .collect();
                    if !components.is_empty() {
                        msg = msg.components(components);
                    }

                    if let Some(poll_data) = parts.poll.as_ref().and_then(build_poll) {
                        msg = msg.poll(poll_data);
                    }
                }

                channel_id
                    .send_message(&*http, msg)
                    .await
                    .context("failed to broadcast discord rich message")?;
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

                // Include mention and reply-to attribution
                let author = if let Some(referenced) = &message.referenced_message {
                    let reply_author = referenced
                        .author
                        .global_name
                        .as_deref()
                        .unwrap_or(&referenced.author.name);
                    format!(
                        "{display_name} (<@{}>) (replying to {reply_author})",
                        message.author.id
                    )
                } else {
                    format!("{display_name} (<@{}>)", message.author.id)
                };

                HistoryMessage {
                    author,
                    content: resolved_content,
                    is_bot,
                    timestamp: Some(*message.timestamp),
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
    runtime_key: String,
    permissions: Arc<ArcSwap<DiscordPermissions>>,
    http_slot: Arc<RwLock<Option<Arc<Http>>>>,
    bot_user_id_slot: Arc<RwLock<Option<UserId>>>,
    interaction_tokens: Arc<RwLock<HashMap<String, InteractionToken>>>,
    command_sync_active: Arc<std::sync::atomic::AtomicBool>,
}

impl Handler {
    /// Handle a native slash-command interaction: admission checks, defer
    /// (ephemeral for Control commands), then inject as structured
    /// `MessageContent::Command` for the router's shared dispatch path.
    async fn handle_command_interaction(&self, ctx: Context, command: CommandInteraction) {
        let permissions = self.permissions.load();
        let user = &command.user;

        if command.guild_id.is_none()
            && (permissions.dm_allowed_users.is_empty()
                || !permissions.dm_allowed_users.contains(&user.id.get()))
        {
            return;
        }
        if let Some(filter) = &permissions.guild_filter
            && let Some(guild_id) = command.guild_id
            && !filter.contains(&guild_id.get())
        {
            return;
        }

        // Acknowledge before resolving thread metadata through Discord's REST
        // API. Parent lookup can be slow, but interactions must be deferred
        // within three seconds.
        let is_control = crate::commands::REGISTRY
            .resolve(&command.data.name)
            .is_some_and(|def| matches!(def.handler, crate::commands::CommandHandler::Control(_)));
        let defer = if is_control {
            CreateInteractionResponse::Defer(
                CreateInteractionResponseMessage::new().ephemeral(true),
            )
        } else {
            CreateInteractionResponse::Defer(CreateInteractionResponseMessage::new())
        };
        if let Err(error) = command.create_response(&ctx.http, defer).await {
            tracing::warn!(%error, command = %command.data.name, "failed to defer slash command");
            return;
        }

        let parent_channel_id = if command.guild_id.is_some() {
            command
                .channel_id
                .to_channel(&ctx.http)
                .await
                .ok()
                .and_then(|channel| channel.guild().and_then(|channel| channel.parent_id))
                .map(|parent_id| parent_id.get())
        } else {
            None
        };
        if let Some(guild_id) = command.guild_id
            && let Some(allowed_channels) = permissions.channel_filter.get(&guild_id.get())
            && !allowed_channels.is_empty()
        {
            let direct_match = allowed_channels.contains(&command.channel_id.get());
            let parent_match = !direct_match
                && parent_channel_id.is_some_and(|parent_id| allowed_channels.contains(&parent_id));
            if !discord_channel_is_allowed(allowed_channels, command.channel_id.get(), parent_match)
            {
                let edit = EditInteractionResponse::new()
                    .content("this command isn't available in this channel");
                if let Err(error) = command.edit_response(&ctx.http, edit).await {
                    tracing::warn!(%error, command = %command.data.name, "failed to resolve denied slash command");
                }
                return;
            }
        }

        let interaction_id = command.id.to_string();
        {
            let mut tokens = self.interaction_tokens.write().await;
            tokens.retain(|_, entry| entry.created.elapsed() < INTERACTION_TOKEN_TTL);
            tokens.insert(
                interaction_id.clone(),
                InteractionToken {
                    token: command.token.clone(),
                    created: Instant::now(),
                },
            );
        }

        let args = command
            .data
            .options
            .first()
            .and_then(|option| match &option.value {
                CommandDataOptionValue::String(value) => Some(value.clone()),
                _ => None,
            })
            .unwrap_or_default();

        let base_conversation_id = match command.guild_id {
            Some(guild_id) => format!("discord:{}:{}", guild_id, command.channel_id),
            None => format!("discord:dm:{}", user.id),
        };
        let conversation_id =
            apply_runtime_adapter_to_conversation_id(&self.runtime_key, base_conversation_id);

        let mut metadata = HashMap::new();
        metadata.insert(
            "discord_channel_id".into(),
            serde_json::Value::Number(command.channel_id.get().into()),
        );
        if let Some(guild_id) = command.guild_id {
            metadata.insert(
                "discord_guild_id".into(),
                serde_json::Value::Number(guild_id.get().into()),
            );
        }
        if let Some(parent_channel_id) = parent_channel_id {
            metadata.insert(
                "discord_parent_channel_id".into(),
                serde_json::Value::Number(parent_channel_id.into()),
            );
        }
        // A command invocation addresses the bot directly.
        metadata.insert("discord_mentioned_bot".into(), true.into());
        metadata.insert("discord_reply_to_bot".into(), false.into());
        metadata.insert("discord_mentions_or_replies_to_bot".into(), true.into());

        let formatted_author = format!("{} (<@{}>)", user.name, user.id);
        metadata.insert(
            "discord_user_id".into(),
            serde_json::Value::Number(user.id.get().into()),
        );
        metadata.insert(
            "sender_display_name".into(),
            serde_json::Value::String(formatted_author.clone()),
        );

        let inbound = InboundMessage {
            id: interaction_id,
            source: "discord".into(),
            adapter: Some(self.runtime_key.clone()),
            conversation_id,
            sender_id: user.id.to_string(),
            agent_id: None,
            content: MessageContent::Command {
                name: command.data.name.clone(),
                args,
            },
            timestamp: chrono::Utc::now(),
            metadata,
            formatted_author: Some(formatted_author),
        };

        if let Err(error) = self.inbound_tx.send(inbound).await {
            tracing::warn!(
                %error,
                "failed to send inbound slash command from Discord (receiver dropped)"
            );
        }
    }
}

#[async_trait]
impl EventHandler for Handler {
    async fn ready(&self, ctx: Context, ready: Ready) {
        tracing::info!(bot_name = %ready.user.name, "discord connected");

        *self.http_slot.write().await = Some(ctx.http.clone());
        *self.bot_user_id_slot.write().await = Some(ready.user.id);
        tracing::info!(guild_count = ready.guilds.len(), "discord guilds available");

        // Register application commands from the registry. `ready` re-fires
        // on reconnect; the sync is diff-only so re-runs are no-ops, and the
        // guard just prevents overlapping syncs.
        if !self
            .command_sync_active
            .swap(true, std::sync::atomic::Ordering::AcqRel)
        {
            let http = ctx.http.clone();
            let permissions = self.permissions.clone();
            let guard = self.command_sync_active.clone();
            tokio::spawn(async move {
                sync_application_commands(&http, &permissions).await;
                guard.store(false, std::sync::atomic::Ordering::Release);
            });
        }
    }

    async fn message(&self, ctx: Context, message: Message) {
        // Always ignore our own messages to prevent self-response loops
        let bot_user_id = *self.bot_user_id_slot.read().await;
        if bot_user_id.is_some_and(|id| message.author.id == id) {
            return;
        }

        // Load a snapshot of the current permissions (hot-reloadable)
        let permissions = self.permissions.load();

        // Filter other bots unless explicitly allowed
        if message.author.bot && !permissions.allow_bot_messages {
            return;
        }

        // DM filter: if no guild_id, it's a DM — only allow listed users
        if message.guild_id.is_none()
            && (permissions.dm_allowed_users.is_empty()
                || !permissions
                    .dm_allowed_users
                    .contains(&message.author.id.get()))
        {
            return;
        }

        if let Some(filter) = &permissions.guild_filter
            && let Some(guild_id) = message.guild_id
            && !filter.contains(&guild_id.get())
        {
            return;
        }

        let conversation_id = build_conversation_id(&self.runtime_key, &message);
        let content = extract_content(&message);
        let (metadata, formatted_author) = build_metadata(&ctx, &message, bot_user_id).await;

        // Channel filter: allow if the channel ID or its parent (for threads) is in the allowlist
        if let Some(guild_id) = message.guild_id
            && let Some(allowed_channels) = permissions.channel_filter.get(&guild_id.get())
            && !allowed_channels.is_empty()
        {
            let parent_channel_id = metadata
                .get("discord_parent_channel_id")
                .and_then(|v| v.as_u64());

            let parent_match = parent_channel_id.is_some_and(|pid| allowed_channels.contains(&pid));

            if !discord_channel_is_allowed(allowed_channels, message.channel_id.get(), parent_match)
            {
                return;
            }
        }

        let inbound = InboundMessage {
            id: message.id.to_string(),
            source: "discord".into(),
            adapter: Some(self.runtime_key.clone()),
            conversation_id,
            sender_id: message.author.id.to_string(),
            agent_id: None,
            content,
            timestamp: *message.timestamp,
            metadata,
            formatted_author: Some(formatted_author),
        };

        if let Err(error) = self.inbound_tx.send(inbound).await {
            tracing::warn!(
                %error,
                "failed to send inbound message from Discord (receiver dropped)"
            );
        }
    }

    async fn interaction_create(&self, ctx: Context, interaction: Interaction) {
        let component = match interaction {
            Interaction::Component(c) => c,
            Interaction::Command(command) => {
                self.handle_command_interaction(ctx, command).await;
                return;
            }
            _ => return,
        };

        // Acknowledge the interaction immediately to prevent "This interaction failed" in the UI.
        // We use Defer to indicate we've received it and might edit the message soon.
        if let Err(error) = component
            .create_response(
                &ctx.http,
                CreateInteractionResponse::Defer(CreateInteractionResponseMessage::new()),
            )
            .await
        {
            tracing::warn!(%error, "failed to acknowledge interaction");
        }

        let user = &component.user;
        let permissions = self.permissions.load();

        if component.guild_id.is_none()
            && (permissions.dm_allowed_users.is_empty()
                || !permissions.dm_allowed_users.contains(&user.id.get()))
        {
            return;
        }

        if let Some(filter) = &permissions.guild_filter
            && let Some(guild_id) = component.guild_id
            && !filter.contains(&guild_id.get())
        {
            return;
        }

        let base_conversation_id = match component.guild_id {
            Some(guild_id) => format!("discord:{}:{}", guild_id, component.channel_id),
            None => format!("discord:dm:{}", user.id),
        };
        let conversation_id =
            apply_runtime_adapter_to_conversation_id(&self.runtime_key, base_conversation_id);

        let values = match &component.data.kind {
            serenity::all::ComponentInteractionDataKind::StringSelect { values } => values.clone(),
            _ => Vec::new(),
        };

        let content = MessageContent::Interaction {
            action_id: component.data.custom_id.clone(),
            block_id: None,
            values,
            label: None,
            message_ts: Some(component.message.id.get().to_string()),
        };

        let mut metadata = HashMap::new();
        metadata.insert(
            "discord_channel_id".into(),
            serde_json::Value::Number(component.channel_id.get().into()),
        );
        metadata.insert(
            "discord_message_id".into(),
            serde_json::Value::Number(component.message.id.get().into()),
        );
        let discord_mentioned_bot = false;
        let discord_reply_to_bot = true;
        metadata.insert("discord_mentioned_bot".into(), discord_mentioned_bot.into());
        metadata.insert("discord_reply_to_bot".into(), discord_reply_to_bot.into());
        metadata.insert(
            "discord_mentions_or_replies_to_bot".into(),
            (discord_mentioned_bot || discord_reply_to_bot).into(),
        );
        if let Some(guild_id) = component.guild_id {
            metadata.insert(
                "discord_guild_id".into(),
                serde_json::Value::Number(guild_id.get().into()),
            );
        }

        let formatted_author = format!("{} (<@{}>)", user.name, user.id);
        metadata.insert(
            "discord_user_id".into(),
            serde_json::Value::Number(user.id.get().into()),
        );
        metadata.insert(
            "sender_display_name".into(),
            serde_json::Value::String(formatted_author.clone()),
        );

        let inbound = InboundMessage {
            id: component.id.to_string(), // Use interaction ID to ensure uniqueness
            source: "discord".into(),
            adapter: Some(self.runtime_key.clone()),
            conversation_id,
            sender_id: user.id.to_string(),
            agent_id: None,
            content,
            timestamp: chrono::Utc::now(),
            metadata,
            formatted_author: Some(formatted_author),
        };

        if let Err(error) = self.inbound_tx.send(inbound).await {
            tracing::warn!(
                %error,
                "failed to send inbound interaction from Discord (receiver dropped)"
            );
        }
    }
}

fn discord_channel_is_allowed(
    allowed_channels: &[u64],
    channel_id: u64,
    parent_matches: bool,
) -> bool {
    allowed_channels.contains(&channel_id) || parent_matches
}

/// Discord application-command field limits (CHAT_INPUT).
const DISCORD_NAME_MAX: usize = 32;
const DISCORD_DESCRIPTION_MAX: usize = 100;
const DISCORD_CHOICE_CAP: usize = 25;
const DISCORD_CHOICE_VALUE_MAX: usize = 100;

/// Why a spec can't be expressed as a Discord application command, if it
/// can't. One invalid entry in a bulk `set_commands` call fails the whole
/// batch, so specs are validated up front and invalid ones skipped.
fn discord_spec_violation(spec: &crate::commands::native::NativeCommandSpec) -> Option<String> {
    use crate::commands::native::NativeArg;

    // Registry names are ASCII; Discord additionally allows lowercase
    // unicode letters, which no command uses.
    let name_valid = !spec.name.is_empty()
        && spec.name.chars().count() <= DISCORD_NAME_MAX
        && spec
            .name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_');
    if !name_valid {
        return Some(format!(
            "name must be 1-{DISCORD_NAME_MAX} lowercase [a-z0-9_-] characters"
        ));
    }
    if let Some(NativeArg::Choice { options }) = &spec.arg {
        if options.len() > DISCORD_CHOICE_CAP {
            return Some(format!(
                "{} choices exceed discord's cap of {DISCORD_CHOICE_CAP}",
                options.len()
            ));
        }
        // A truncated choice value would dispatch different args than the
        // user picked, so over-long choices invalidate the command instead.
        if options
            .iter()
            .any(|option| option.is_empty() || option.chars().count() > DISCORD_CHOICE_VALUE_MAX)
        {
            return Some(format!(
                "choice values must be 1-{DISCORD_CHOICE_VALUE_MAX} characters"
            ));
        }
    }
    None
}

/// Clamp a description to Discord's 1-100 character range, falling back to
/// the command name when empty.
fn normalize_discord_description(description: &str, command_name: &str) -> String {
    let normalized: String = description.chars().take(DISCORD_DESCRIPTION_MAX).collect();
    if normalized.is_empty() {
        format!("/{command_name}")
    } else {
        normalized
    }
}

/// Build serenity command definitions from the registry's native specs.
/// Returned specs are normalized to Discord limits and match the built
/// commands one-to-one, so the diff comparison sees exactly what was
/// registered.
fn build_discord_create_commands() -> (
    Vec<CreateCommand>,
    Vec<crate::commands::native::NativeCommandSpec>,
    usize,
) {
    use crate::commands::native::discord_commands;

    let (specs, dropped) = discord_commands();
    let specs: Vec<_> = specs
        .into_iter()
        .filter(|spec| match discord_spec_violation(spec) {
            Some(reason) => {
                tracing::warn!(
                    command = %spec.name,
                    reason,
                    "command cannot be registered as a discord application command, skipping"
                );
                false
            }
            None => true,
        })
        .map(|mut spec| {
            spec.description = normalize_discord_description(&spec.description, &spec.name);
            spec
        })
        .collect();
    let commands = specs
        .iter()
        .map(|spec| {
            let mut command = CreateCommand::new(&spec.name).description(&spec.description);
            if let Some((name, description, required, choices)) = expected_option(spec) {
                let mut option =
                    CreateCommandOption::new(CommandOptionType::String, name, description)
                        .required(required);
                for choice in choices {
                    option = option.add_string_choice(&choice, &choice);
                }
                command = command.add_option(option);
            }
            command
        })
        .collect();
    (commands, specs, dropped)
}

/// The option a spec maps to: (name, description, required, choices).
/// Shared between registration and the diff comparison so they can't drift.
fn expected_option(
    spec: &crate::commands::native::NativeCommandSpec,
) -> Option<(String, String, bool, Vec<String>)> {
    use crate::commands::native::NativeArg;
    match &spec.arg {
        None => None,
        Some(NativeArg::Text { hint, required }) => Some((
            "input".to_string(),
            {
                let description: String = hint.chars().take(DISCORD_DESCRIPTION_MAX).collect();
                if description.is_empty() {
                    "input".to_string()
                } else {
                    description
                }
            },
            *required,
            Vec::new(),
        )),
        Some(NativeArg::Choice { options }) => Some((
            "value".to_string(),
            {
                let description: String = format!("one of: {}", options.join(", "))
                    .chars()
                    .take(DISCORD_DESCRIPTION_MAX)
                    .collect();
                description
            },
            false,
            options.clone(),
        )),
    }
}

/// Whether the live command set already matches the registry specs.
/// Comparing name, description, and option shape keeps re-registration a
/// no-op on every reconnect.
fn command_set_matches(
    existing: &[ApplicationCommand],
    specs: &[crate::commands::native::NativeCommandSpec],
) -> bool {
    type Normalized = (String, String, Option<(String, String, bool, Vec<String>)>);

    let mut live: Vec<Normalized> = existing
        .iter()
        .map(|command| {
            let option = command.options.first().map(|option| {
                (
                    option.name.clone(),
                    option.description.clone(),
                    option.required,
                    option
                        .choices
                        .iter()
                        .map(|choice| choice.name.clone())
                        .collect::<Vec<_>>(),
                )
            });
            (command.name.clone(), command.description.clone(), option)
        })
        .collect();
    let mut wanted: Vec<Normalized> = specs
        .iter()
        .map(|spec| {
            (
                spec.name.clone(),
                spec.description.clone(),
                expected_option(spec),
            )
        })
        .collect();
    live.sort();
    wanted.sort();
    live == wanted
}

/// Diff-only application-command sync. Guild-scoped when bindings declare
/// guilds (instant propagation, scoped to served guilds), global otherwise.
async fn sync_application_commands(
    http: &Arc<Http>,
    permissions: &Arc<ArcSwap<DiscordPermissions>>,
) {
    let (commands, specs, dropped) = build_discord_create_commands();
    if dropped > 0 {
        tracing::warn!(
            dropped,
            cap = crate::commands::native::DISCORD_COMMAND_CAP,
            "discord command cap exceeded; trailing registry commands were not registered"
        );
    }

    let guild_filter = permissions.load().guild_filter.clone();
    match guild_filter {
        Some(guild_ids) => {
            // Every configured guild is synced, not just the ones present
            // in a READY payload — a served guild missing from one gateway
            // session would otherwise get no commands until a reconnect
            // happens to include it. Per-guild failures (the bot not being
            // a member yet, transient API errors) are logged and the loop
            // continues with the remaining guilds. The filter can repeat a
            // guild (one binding per channel set), so duplicates are synced
            // once.
            let mut all_guilds_synced = true;
            let mut seen_guilds = std::collections::HashSet::new();
            for guild_id in guild_ids
                .iter()
                .copied()
                .filter(|guild_id| seen_guilds.insert(*guild_id))
                .map(GuildId::new)
            {
                let in_sync = match guild_id.get_commands(http).await {
                    Ok(existing) => command_set_matches(&existing, &specs),
                    Err(error) => {
                        tracing::warn!(%error, guild_id = %guild_id, "failed to fetch guild commands");
                        false
                    }
                };
                if in_sync {
                    tracing::debug!(guild_id = %guild_id, "discord commands already in sync");
                    continue;
                }
                match guild_id.set_commands(http, commands.clone()).await {
                    Ok(registered) => {
                        tracing::info!(
                            guild_id = %guild_id,
                            count = registered.len(),
                            "discord guild commands registered"
                        );
                    }
                    Err(error) => {
                        all_guilds_synced = false;
                        tracing::warn!(%error, guild_id = %guild_id, "failed to register guild commands");
                    }
                }
            }

            // Stale globals are cleared only after every configured guild is
            // confirmed in sync — clearing first would leave a guild whose
            // registration then failed with no commands at all until a later
            // reconnect. A guild that failed keeps the globals as a fallback;
            // the next `ready` re-sync retries both.
            if !all_guilds_synced {
                tracing::warn!(
                    "leaving global discord commands in place until every configured guild syncs"
                );
                return;
            }
            match ApplicationCommand::get_global_commands(http).await {
                Ok(global) if !global.is_empty() => {
                    if let Err(error) =
                        ApplicationCommand::set_global_commands(http, Vec::new()).await
                    {
                        tracing::warn!(%error, "failed to clear stale global discord commands");
                    }
                }
                Ok(_) => {}
                Err(error) => {
                    tracing::warn!(%error, "failed to fetch global discord commands");
                }
            }
        }
        None => {
            match ApplicationCommand::get_global_commands(http).await {
                Ok(existing) if command_set_matches(&existing, &specs) => {
                    tracing::debug!("global discord commands already in sync");
                    return;
                }
                Ok(_) => {}
                Err(error) => {
                    tracing::warn!(%error, "failed to fetch global discord commands");
                    return;
                }
            }
            match ApplicationCommand::set_global_commands(http, commands).await {
                Ok(registered) => {
                    tracing::info!(
                        count = registered.len(),
                        "global discord commands registered"
                    );
                }
                Err(error) => {
                    tracing::warn!(%error, "failed to register global discord commands");
                }
            }
        }
    }
}

fn is_mention_or_reply_to_bot(message: &Message, bot_user_id: Option<UserId>) -> bool {
    is_mention_to_bot(message, bot_user_id) || is_reply_to_bot(message, bot_user_id)
}

fn is_mention_to_bot(message: &Message, bot_user_id: Option<UserId>) -> bool {
    let Some(bot_id) = bot_user_id else {
        return false;
    };

    message.mentions.iter().any(|user| user.id == bot_id)
}

fn is_reply_to_bot(message: &Message, bot_user_id: Option<UserId>) -> bool {
    let Some(bot_id) = bot_user_id else {
        return false;
    };

    message
        .referenced_message
        .as_ref()
        .is_some_and(|referenced| referenced.author.id == bot_id)
}

// -- Helper functions --

fn build_conversation_id(runtime_key: &str, message: &Message) -> String {
    let base_conversation_id = match message.guild_id {
        Some(guild_id) => format!("discord:{}:{}", guild_id, message.channel_id),
        None => format!("discord:dm:{}", message.author.id),
    };

    apply_runtime_adapter_to_conversation_id(runtime_key, base_conversation_id)
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
                auth_header: None,
                pre_saved_id: None,
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

async fn build_metadata(
    ctx: &Context,
    message: &Message,
    bot_user_id: Option<UserId>,
) -> (HashMap<String, serde_json::Value>, String) {
    let mut metadata = HashMap::new();
    metadata.insert("discord_channel_id".into(), message.channel_id.get().into());
    metadata.insert("discord_message_id".into(), message.id.get().into());
    metadata.insert(
        crate::metadata_keys::MESSAGE_ID.into(),
        serde_json::Value::String(message.id.get().to_string()),
    );
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
    metadata.insert("sender_display_name".into(), display_name.clone().into());
    metadata.insert("sender_id".into(), message.author.id.get().into());
    metadata.insert(
        "discord_user_mention".into(),
        serde_json::Value::String(format!("<@{}>", message.author.id)),
    );

    // Platform-formatted author for LLM context
    let formatted_author = format!("{} (<@{}>)", display_name, message.author.id);

    if message.author.bot {
        metadata.insert("sender_is_bot".into(), true.into());
    }

    if let Some(guild_id) = message.guild_id {
        metadata.insert("discord_guild_id".into(), guild_id.get().into());

        // Try to get guild name
        if let Ok(guild) = guild_id.to_partial_guild(&ctx.http).await {
            metadata.insert("discord_guild_name".into(), guild.name.clone().into());
            metadata.insert(crate::metadata_keys::SERVER_NAME.into(), guild.name.into());
        }
    }

    // Try to get channel name and detect threads
    if let Ok(channel) = message.channel_id.to_channel(&ctx.http).await
        && let Some(guild_channel) = channel.guild()
    {
        metadata.insert(
            "discord_channel_name".into(),
            guild_channel.name.clone().into(),
        );
        metadata.insert(
            crate::metadata_keys::CHANNEL_NAME.into(),
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
            format!(
                "{}...",
                &reply_content[..reply_content.floor_char_boundary(200)]
            )
        } else {
            reply_content
        };
        metadata.insert("reply_to_content".into(), truncated.clone().into());
        metadata.insert(crate::metadata_keys::REPLY_TO_TEXT.into(), truncated.into());
    }

    metadata.insert(
        "discord_mentions_or_replies_to_bot".into(),
        is_mention_or_reply_to_bot(message, bot_user_id).into(),
    );
    metadata.insert(
        "discord_mentioned_bot".into(),
        is_mention_to_bot(message, bot_user_id).into(),
    );
    metadata.insert(
        "discord_reply_to_bot".into(),
        is_reply_to_bot(message, bot_user_id).into(),
    );

    (metadata, formatted_author)
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

        let safe_max = {
            let mut i = max_len.min(remaining.len());
            while !remaining.is_char_boundary(i) {
                i -= 1;
            }
            i
        };

        let split_at = remaining[..safe_max]
            .rfind('\n')
            .or_else(|| remaining[..safe_max].rfind(' '))
            .unwrap_or(safe_max);

        chunks.push(remaining[..split_at].to_string());
        remaining = remaining[split_at..].trim_start();
    }

    chunks
}

// --- Rich Message Builders ---

fn build_embed(card: &crate::Card) -> CreateEmbed {
    let mut embed = CreateEmbed::new();

    if let Some(title) = &card.title {
        embed = embed.title(title);
    }
    if let Some(desc) = &card.description {
        embed = embed.description(desc);
    }
    if let Some(color) = card.color {
        embed = embed.color(color);
    }
    if let Some(url) = &card.url {
        embed = embed.url(url);
    }
    if let Some(footer) = &card.footer {
        let footer_text = footer.text.trim();
        if !footer_text.is_empty() {
            let mut footer_builder = CreateEmbedFooter::new(footer_text);
            if let Some(icon_url) = &footer.icon_url {
                footer_builder = footer_builder.icon_url(icon_url);
            }
            embed = embed.footer(footer_builder);
        }
    }
    if let Some(thumbnail) = &card.thumbnail {
        embed = embed.thumbnail(&thumbnail.url);
    }
    if let Some(image) = &card.image {
        embed = embed.image(&image.url);
    }
    if let Some(author) = &card.author {
        let author_name = author.name.trim();
        if !author_name.is_empty() {
            let mut author_builder = CreateEmbedAuthor::new(author_name);
            if let Some(url) = &author.url {
                author_builder = author_builder.url(url);
            }
            if let Some(icon_url) = &author.icon_url {
                author_builder = author_builder.icon_url(icon_url);
            }
            embed = embed.author(author_builder);
        }
    }
    if let Some(timestamp) = &card.timestamp {
        match timestamp.parse::<Timestamp>() {
            Ok(ts) => embed = embed.timestamp(ts),
            Err(e) => tracing::warn!(timestamp, %e, "invalid ISO 8601 timestamp in card, skipping"),
        }
    }

    for (i, field) in card.fields.iter().enumerate() {
        if i >= 25 {
            break; // Discord limit: max 25 fields per embed
        }
        embed = embed.field(&field.name, &field.value, field.inline);
    }

    embed
}

fn build_action_row(elements: &crate::InteractiveElements) -> CreateActionRow {
    match elements {
        crate::InteractiveElements::Buttons { buttons } => {
            let mut discord_buttons = Vec::new();
            for (i, btn) in buttons.iter().enumerate() {
                if i >= 5 {
                    break; // Discord limit: max 5 buttons per action row
                }

                let b = match btn.style {
                    crate::ButtonStyle::Link => {
                        let Some(url) = btn.url.as_deref() else {
                            continue;
                        };
                        CreateButton::new_link(url).label(&btn.label)
                    }
                    style => {
                        let serenity_style = match style {
                            crate::ButtonStyle::Primary => ButtonStyle::Primary,
                            crate::ButtonStyle::Secondary => ButtonStyle::Secondary,
                            crate::ButtonStyle::Success => ButtonStyle::Success,
                            crate::ButtonStyle::Danger => ButtonStyle::Danger,
                            _ => ButtonStyle::Primary, // fallback
                        };
                        let custom_id = btn.custom_id.as_deref().unwrap_or("btn");
                        // Discord limit: custom_id max 100 characters.
                        let custom_id = &custom_id[..custom_id.floor_char_boundary(100)];
                        CreateButton::new(custom_id)
                            .label(&btn.label)
                            .style(serenity_style)
                    }
                };

                discord_buttons.push(b);
            }
            CreateActionRow::Buttons(discord_buttons)
        }
        crate::InteractiveElements::Select { select } => {
            let mut options = Vec::new();
            for opt in &select.options {
                let mut discord_opt = CreateSelectMenuOption::new(&opt.label, &opt.value);
                if let Some(desc) = &opt.description {
                    discord_opt = discord_opt.description(desc);
                }
                // (Emoji not mapped for now)
                options.push(discord_opt);
            }

            // Discord limit: custom_id max 100 characters.
            let custom_id = &select.custom_id[..select.custom_id.floor_char_boundary(100)];

            let mut discord_select =
                CreateSelectMenu::new(custom_id, CreateSelectMenuKind::String { options });
            if let Some(placeholder) = &select.placeholder {
                discord_select = discord_select.placeholder(placeholder);
            }

            CreateActionRow::SelectMenu(discord_select)
        }
    }
}

struct RichMessageParts<'a> {
    text: String,
    cards: &'a [crate::Card],
    interactive_elements: &'a [crate::InteractiveElements],
    poll: Option<crate::Poll>,
    dropped_invalid_poll: bool,
}

fn prepare_rich_message_parts<'a>(
    mut text: String,
    cards: &'a [crate::Card],
    interactive_elements: &'a [crate::InteractiveElements],
    poll: Option<&crate::Poll>,
) -> RichMessageParts<'a> {
    // Derive a plaintext fallback from cards when text is empty so the message
    // is never blank for notifications, logs, or non-rich adapters.
    if text.trim().is_empty() {
        let derived = crate::OutboundResponse::text_from_cards(cards);
        if !derived.trim().is_empty() {
            text = derived;
        }
    }

    let cards = if cards.len() > 10 {
        tracing::warn!(
            count = cards.len(),
            "truncating cards to Discord embed limit (10)"
        );
        &cards[..10]
    } else {
        cards
    };

    let interactive_elements = if interactive_elements.len() > 5 {
        tracing::warn!(
            count = interactive_elements.len(),
            "truncating interactive elements to Discord action row limit (5)"
        );
        &interactive_elements[..5]
    } else {
        interactive_elements
    };

    let had_poll = poll.is_some();
    let poll = poll.filter(|poll| build_poll(poll).is_some()).cloned();
    let dropped_invalid_poll = had_poll && poll.is_none();

    RichMessageParts {
        text,
        cards,
        interactive_elements,
        poll,
        dropped_invalid_poll,
    }
}

fn build_poll(
    poll: &crate::Poll,
) -> Option<serenity::builder::CreatePoll<serenity::builder::create_poll::Ready>> {
    let question = poll.question.trim();
    if question.is_empty() {
        return None;
    }

    // Discord limits: max 10 answers
    let answers: Vec<_> = poll
        .answers
        .iter()
        .map(|answer| answer.trim())
        .filter(|answer| !answer.is_empty())
        .take(10)
        .map(|answer| CreatePollAnswer::new().text(answer))
        .collect();

    if answers.len() < 2 {
        return None;
    }

    // Duration must be at least 1 hour, usually up to 720 hours (30 days).
    // The builder just takes std::time::Duration but it has specific allowed values.
    let hours = poll.duration_hours.clamp(1, 720);

    let mut p = CreatePoll::new()
        .question(question)
        .answers(answers)
        .duration(std::time::Duration::from_secs((hours as u64) * 3600));

    if poll.allow_multiselect {
        p = p.allow_multiselect();
    }

    Some(p)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_commands_share_message_channel_admission() {
        let allowed = vec![10, 20];
        assert!(discord_channel_is_allowed(&allowed, 10, false));
        assert!(discord_channel_is_allowed(&allowed, 30, true));
        assert!(!discord_channel_is_allowed(&allowed, 30, false));
    }
    use crate::{Button, ButtonStyle, Card, CardField, InteractiveElements, Poll};

    #[test]
    fn discord_specs_satisfy_platform_limits() {
        let (commands, specs, _) = build_discord_create_commands();
        assert_eq!(commands.len(), specs.len());
        for spec in &specs {
            assert!(
                discord_spec_violation(spec).is_none(),
                "spec /{} violates discord limits",
                spec.name
            );
            assert!(
                (1..=DISCORD_DESCRIPTION_MAX).contains(&spec.description.chars().count()),
                "discord description length for /{}",
                spec.name
            );
            if let Some((_, description, _, choices)) = expected_option(spec) {
                assert!(
                    (1..=DISCORD_DESCRIPTION_MAX).contains(&description.chars().count()),
                    "discord option description length for /{}",
                    spec.name
                );
                assert!(
                    choices.len() <= DISCORD_CHOICE_CAP,
                    "discord choice cap for /{}",
                    spec.name
                );
            }
        }
    }

    #[test]
    fn discord_validation_drops_no_registry_command() {
        // Every command the registry exposes on Discord must survive
        // validation — a violation here means a registry entry silently
        // disappears from Discord instead of failing the build.
        let (specs, _) = crate::commands::native::discord_commands();
        let (_, validated, _) = build_discord_create_commands();
        let expected: Vec<&str> = specs.iter().map(|spec| spec.name.as_str()).collect();
        let actual: Vec<&str> = validated.iter().map(|spec| spec.name.as_str()).collect();
        assert_eq!(actual, expected);
    }

    #[test]
    fn discord_spec_violation_flags_invalid_specs() {
        use crate::commands::native::{NativeArg, NativeCommandSpec};

        let valid = NativeCommandSpec {
            name: "mention-only".into(),
            description: "only respond when mentioned".into(),
            arg: None,
        };
        assert!(discord_spec_violation(&valid).is_none());

        let uppercase = NativeCommandSpec {
            name: "Status".into(),
            ..valid.clone()
        };
        assert!(discord_spec_violation(&uppercase).is_some());

        let too_long = NativeCommandSpec {
            name: "a".repeat(DISCORD_NAME_MAX + 1),
            ..valid.clone()
        };
        assert!(discord_spec_violation(&too_long).is_some());

        let too_many_choices = NativeCommandSpec {
            arg: Some(NativeArg::Choice {
                options: (0..DISCORD_CHOICE_CAP + 1)
                    .map(|i| format!("option-{i}"))
                    .collect(),
            }),
            ..valid.clone()
        };
        assert!(discord_spec_violation(&too_many_choices).is_some());

        let oversized_choice = NativeCommandSpec {
            arg: Some(NativeArg::Choice {
                options: vec!["x".repeat(DISCORD_CHOICE_VALUE_MAX + 1)],
            }),
            ..valid
        };
        assert!(discord_spec_violation(&oversized_choice).is_some());
    }

    #[test]
    fn discord_description_normalization_clamps_and_falls_back() {
        let clamped = normalize_discord_description(&"d".repeat(300), "status");
        assert_eq!(clamped.chars().count(), DISCORD_DESCRIPTION_MAX);
        assert_eq!(normalize_discord_description("", "status"), "/status");
    }

    #[test]
    fn test_build_embed_limits() {
        let mut card = Card::default();
        for i in 0..30 {
            card.fields.push(CardField {
                name: format!("Field {}", i),
                value: "Value".into(),
                inline: false,
            });
        }

        // build_embed should limit fields to 25
        let _embed = build_embed(&card);
        // Serenity 0.12 CreateEmbed fields are stored internally, but we can't inspect them directly easily
        // We just ensure it doesn't panic.
        // we'd need to inspect the JSON payload to really test, but it compiles and runs safely.
    }

    #[test]
    fn test_build_action_row_button_limits() {
        let mut buttons = Vec::new();
        for i in 0..10 {
            buttons.push(Button {
                label: format!("Btn {}", i),
                custom_id: Some(format!("id_{}", i)),
                style: ButtonStyle::Primary,
                url: None,
            });
        }

        let row = InteractiveElements::Buttons { buttons };
        let action_row = build_action_row(&row);
        match action_row {
            CreateActionRow::Buttons(btns) => {
                assert_eq!(btns.len(), 5, "Discord limit: max 5 buttons per action row");
            }
            _ => panic!("Expected Buttons"),
        }
    }

    #[test]
    fn test_build_poll_limits() {
        let mut poll = Poll {
            question: "Question?".into(),
            answers: Vec::new(),
            allow_multiselect: false,
            duration_hours: 1000, // Exceeds 720 limit
        };
        for i in 0..15 {
            poll.answers.push(format!("Answer {}", i));
        }

        // build_poll should limit answers to 10 and duration to 720
        let built = build_poll(&poll);
        assert!(built.is_some());
        // Again, can't easily inspect CreatePoll fields, but we verify it runs.
    }

    #[test]
    fn test_build_poll_rejects_blank_question() {
        let poll = Poll {
            question: "   ".into(),
            answers: vec!["Yes".into(), "No".into()],
            allow_multiselect: false,
            duration_hours: 24,
        };

        assert!(build_poll(&poll).is_none());
    }

    #[test]
    fn test_build_poll_rejects_single_non_empty_answer() {
        let poll = Poll {
            question: "Question?".into(),
            answers: vec!["Yes".into(), "   ".into()],
            allow_multiselect: false,
            duration_hours: 24,
        };

        assert!(build_poll(&poll).is_none());
    }

    #[test]
    fn test_prepare_rich_message_parts_drops_invalid_poll_but_keeps_text() {
        let poll = Poll {
            question: "   ".into(),
            answers: vec!["Yes".into(), "No".into()],
            allow_multiselect: false,
            duration_hours: 24,
        };

        let parts = prepare_rich_message_parts("plain text reply".into(), &[], &[], Some(&poll));

        assert_eq!(parts.text, "plain text reply");
        assert!(parts.poll.is_none());
        assert!(parts.dropped_invalid_poll);
    }

    #[test]
    fn test_prepare_rich_message_parts_derives_text_fallback_from_cards() {
        let cards = vec![Card {
            title: Some("Status".into()),
            description: Some("All green".into()),
            ..Default::default()
        }];

        let parts = prepare_rich_message_parts(String::new(), &cards, &[], None);

        assert_eq!(parts.text, "Status\n\nAll green");
        assert!(!parts.dropped_invalid_poll);
    }
}
