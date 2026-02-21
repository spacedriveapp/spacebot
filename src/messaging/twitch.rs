//! Twitch chat messaging adapter using twitch-irc.

use crate::config::TwitchPermissions;
use crate::messaging::traits::{InboundStream, Messaging};
use crate::{InboundMessage, MessageContent, OutboundResponse};

use anyhow::Context as _;
use arc_swap::ArcSwap;
use twitch_irc::login::StaticLoginCredentials;
use twitch_irc::message::ServerMessage;
use twitch_irc::{ClientConfig, SecureTCPTransport, TwitchIRCClient};

use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{RwLock, mpsc};

type IrcClient = TwitchIRCClient<SecureTCPTransport, StaticLoginCredentials>;

/// Twitch chat adapter state.
pub struct TwitchAdapter {
    username: String,
    oauth_token: String,
    channels: Vec<String>,
    trigger_prefix: Option<String>,
    permissions: Arc<ArcSwap<TwitchPermissions>>,
    client: Arc<RwLock<Option<IrcClient>>>,
    shutdown_tx: Arc<RwLock<Option<mpsc::Sender<()>>>>,
}

/// Twitch chat messages are limited to 500 characters.
const MAX_MESSAGE_LENGTH: usize = 500;

impl TwitchAdapter {
    pub fn new(
        username: impl Into<String>,
        oauth_token: impl Into<String>,
        channels: Vec<String>,
        trigger_prefix: Option<String>,
        permissions: Arc<ArcSwap<TwitchPermissions>>,
    ) -> Self {
        Self {
            username: username.into(),
            oauth_token: oauth_token.into(),
            channels,
            trigger_prefix,
            permissions,
            client: Arc::new(RwLock::new(None)),
            shutdown_tx: Arc::new(RwLock::new(None)),
        }
    }
}

impl Messaging for TwitchAdapter {
    fn name(&self) -> &str {
        "twitch"
    }

    async fn start(&self) -> crate::Result<InboundStream> {
        let (inbound_tx, inbound_rx) = mpsc::channel(256);
        let (shutdown_tx, mut shutdown_rx) = mpsc::channel::<()>(1);

        *self.shutdown_tx.write().await = Some(shutdown_tx);

        // Strip "oauth:" prefix if the user included it
        let token = self
            .oauth_token
            .strip_prefix("oauth:")
            .unwrap_or(&self.oauth_token)
            .to_string();

        let credentials = StaticLoginCredentials::new(self.username.clone(), Some(token));
        let config = ClientConfig::new_simple(credentials);

        let (mut incoming, client) =
            TwitchIRCClient::<SecureTCPTransport, StaticLoginCredentials>::new(config);

        // Join configured channels
        for channel in &self.channels {
            let channel_login = channel.strip_prefix('#').unwrap_or(channel);
            if let Err(error) = client.join(channel_login.to_owned()) {
                tracing::error!(channel = %channel_login, %error, "failed to join twitch channel");
            }
        }

        tracing::info!(
            username = %self.username,
            channels = ?self.channels,
            "twitch connected"
        );

        *self.client.write().await = Some(client);

        let permissions = self.permissions.clone();
        let bot_username = self.username.to_lowercase();
        let trigger_prefix = self.trigger_prefix.clone();

        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = shutdown_rx.recv() => {
                        tracing::info!("twitch message loop shutting down");
                        break;
                    }
                    message = incoming.recv() => {
                        let Some(message) = message else {
                            tracing::info!("twitch incoming stream ended");
                            break;
                        };

                        let ServerMessage::Privmsg(privmsg) = message else {
                            continue;
                        };

                        // Skip our own messages
                        if privmsg.sender.login.to_lowercase() == bot_username {
                            continue;
                        }

                        let permissions = permissions.load();

                        // Channel filter
                        if let Some(filter) = &permissions.channel_filter
                            && !filter.iter().any(|c| c.eq_ignore_ascii_case(&privmsg.channel_login)) {
                                continue;
                            }

                        // User filter
                        if !permissions.allowed_users.is_empty()
                            && !permissions.allowed_users.iter().any(|u| u.eq_ignore_ascii_case(&privmsg.sender.login))
                        {
                            continue;
                        }

                        let mut text = privmsg.message_text.clone();

                        // Trigger prefix filtering: if configured, only respond to messages
                        // that start with the prefix, and strip the prefix before processing
                        if let Some(ref prefix) = trigger_prefix {
                            if let Some(stripped) = text.strip_prefix(prefix.as_str()) {
                                text = stripped.trim_start().to_string();
                            } else {
                                continue;
                            }
                        }

                        let channel_login = privmsg.channel_login.clone();
                        let conversation_id = format!("twitch:{channel_login}");

                        let mut metadata = HashMap::new();
                        metadata.insert(
                            "twitch_channel".into(),
                            serde_json::Value::String(channel_login),
                        );
                        metadata.insert(
                            "twitch_message_id".into(),
                            serde_json::Value::String(privmsg.message_id.clone()),
                        );
                        metadata.insert(
                            "twitch_user_id".into(),
                            serde_json::Value::String(privmsg.sender.id.clone()),
                        );
                        metadata.insert(
                            "twitch_user_login".into(),
                            serde_json::Value::String(privmsg.sender.login.clone()),
                        );
                        metadata.insert(
                            "sender_display_name".into(),
                            serde_json::Value::String(privmsg.sender.name.clone()),
                        );

                        let formatted_author = format!(
                            "{} ({})",
                            privmsg.sender.name,
                            privmsg.sender.login
                        );

                        let inbound = InboundMessage {
                            id: privmsg.message_id.clone(),
                            source: "twitch".into(),
                            conversation_id,
                            sender_id: privmsg.sender.id.clone(),
                            agent_id: None,
                            content: MessageContent::Text(text),
                            timestamp: privmsg.server_timestamp,
                            metadata,
                            formatted_author: Some(formatted_author),
                        };

                        if let Err(error) = inbound_tx.send(inbound).await {
                            tracing::warn!(
                                %error,
                                "failed to send inbound message from Twitch (receiver dropped)"
                            );
                            return;
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
        let client_guard = self.client.read().await;
        let client = client_guard
            .as_ref()
            .context("twitch client not connected")?;

        let channel = message
            .metadata
            .get("twitch_channel")
            .and_then(|v| v.as_str())
            .context("missing twitch_channel in metadata")?;

        match response {
            OutboundResponse::Text(text) => {
                for chunk in split_message(&text, MAX_MESSAGE_LENGTH) {
                    client
                        .say(channel.to_owned(), chunk)
                        .await
                        .context("failed to send twitch message")?;
                }
            }
            OutboundResponse::RichMessage { text, .. } => {
                for chunk in split_message(&text, MAX_MESSAGE_LENGTH) {
                    client
                        .say(channel.to_owned(), chunk)
                        .await
                        .context("failed to send twitch message")?;
                }
            }
            OutboundResponse::ThreadReply { text, .. } => {
                // Twitch has no threads — reply to the source message instead
                let reply_to_id = message
                    .metadata
                    .get("twitch_message_id")
                    .and_then(|v| v.as_str());

                for chunk in split_message(&text, MAX_MESSAGE_LENGTH) {
                    if let Some(parent_id) = reply_to_id {
                        let reply_ref = (channel, parent_id);
                        client
                            .say_in_reply_to(&reply_ref, chunk)
                            .await
                            .context("failed to send twitch reply")?;
                    } else {
                        client
                            .say(channel.to_owned(), chunk)
                            .await
                            .context("failed to send twitch message")?;
                    }
                }
            }
            OutboundResponse::File {
                filename, caption, ..
            } => {
                // Twitch is text-only — send a note about the file
                let text = match caption {
                    Some(caption) => format!("[File: {filename}] {caption}"),
                    None => format!("[File: {filename}]"),
                };
                client
                    .say(channel.to_owned(), text)
                    .await
                    .context("failed to send twitch file notice")?;
            }
            // Twitch doesn't support message editing, so buffer streaming and
            // send the final result as a single message
            OutboundResponse::StreamStart | OutboundResponse::StreamChunk(_) => {
                // No-op: we can't edit messages in Twitch chat.
                // The StreamEnd with final text is handled by the outbound routing
                // which sends a Text response after StreamEnd.
            }
            OutboundResponse::StreamEnd => {}
            // Reactions, status updates, and Slack-specific variants aren't meaningful in Twitch chat
            OutboundResponse::Reaction(_)
            | OutboundResponse::RemoveReaction(_)
            | OutboundResponse::Status(_) => {}
            OutboundResponse::Ephemeral { text, .. } => {
                // No ephemeral concept in Twitch — send as regular chat message
                client
                    .say(channel.to_owned(), text)
                    .await
                    .context("failed to send ephemeral fallback on twitch")?;
            }
            OutboundResponse::ScheduledMessage { text, .. } => {
                // No scheduled messages on Twitch — send immediately
                client
                    .say(channel.to_owned(), text)
                    .await
                    .context("failed to send scheduled message fallback on twitch")?;
            }
        }

        Ok(())
    }

    async fn broadcast(&self, target: &str, response: OutboundResponse) -> crate::Result<()> {
        let client_guard = self.client.read().await;
        let client = client_guard
            .as_ref()
            .context("twitch client not connected")?;

        if let OutboundResponse::Text(text) = response {
            let channel = target.strip_prefix('#').unwrap_or(target);
            for chunk in split_message(&text, MAX_MESSAGE_LENGTH) {
                client
                    .say(channel.to_owned(), chunk)
                    .await
                    .context("failed to broadcast twitch message")?;
            }
        } else if let OutboundResponse::RichMessage { text, .. } = response {
            let channel = target.strip_prefix('#').unwrap_or(target);
            for chunk in split_message(&text, MAX_MESSAGE_LENGTH) {
                client
                    .say(channel.to_owned(), chunk)
                    .await
                    .context("failed to broadcast twitch message")?;
            }
        }

        Ok(())
    }

    async fn health_check(&self) -> crate::Result<()> {
        let client_guard = self.client.read().await;
        if client_guard.is_none() {
            return Err(anyhow::anyhow!("twitch client not connected").into());
        }
        Ok(())
    }

    async fn shutdown(&self) -> crate::Result<()> {
        // Signal the message loop to stop
        if let Some(tx) = self.shutdown_tx.read().await.as_ref() {
            tx.send(()).await.ok();
        }

        // Drop the client to close all connections
        *self.client.write().await = None;

        tracing::info!("twitch adapter shut down");
        Ok(())
    }
}

/// Split a message into chunks that fit within Twitch's character limit.
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
