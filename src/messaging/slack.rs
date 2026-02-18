//! Slack messaging adapter using slack-morphism.

use crate::config::SlackPermissions;
use crate::messaging::traits::{HistoryMessage, InboundStream, Messaging};
use crate::{InboundMessage, MessageContent, OutboundResponse, StatusUpdate};

use anyhow::Context as _;
use arc_swap::ArcSwap;
use slack_morphism::prelude::*;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{RwLock, mpsc};

/// State shared with socket mode callbacks via `SlackClientEventsUserState`.
struct SlackAdapterState {
    inbound_tx: mpsc::Sender<InboundMessage>,
    permissions: Arc<ArcSwap<SlackPermissions>>,
    bot_token: String,
}

/// Slack adapter state.
pub struct SlackAdapter {
    bot_token: String,
    app_token: String,
    permissions: Arc<ArcSwap<SlackPermissions>>,
    /// Maps InboundMessage.id to the Slack message timestamp (ts) for editing during streaming.
    active_messages: Arc<RwLock<HashMap<String, String>>>,
    shutdown_tx: Arc<RwLock<Option<mpsc::Sender<()>>>>,
}

impl SlackAdapter {
    pub fn new(
        bot_token: impl Into<String>,
        app_token: impl Into<String>,
        permissions: Arc<ArcSwap<SlackPermissions>>,
    ) -> Self {
        Self {
            bot_token: bot_token.into(),
            app_token: app_token.into(),
            permissions,
            active_messages: Arc::new(RwLock::new(HashMap::new())),
            shutdown_tx: Arc::new(RwLock::new(None)),
        }
    }

    /// Create a session for making API calls.
    fn create_session(&self) -> anyhow::Result<(Arc<SlackHyperClient>, SlackApiToken)> {
        let client = Arc::new(SlackClient::new(
            SlackClientHyperConnector::new().context("failed to create slack connector")?,
        ));
        let token = SlackApiToken::new(SlackApiTokenValue(self.bot_token.clone()));
        Ok((client, token))
    }
}

/// Socket mode push event handler. Must be a `fn` pointer (not a closure)
/// because slack-morphism requires `UserCallbackFunction` which is a fn pointer type.
async fn handle_push_event(
    event: SlackPushEventCallback,
    client: Arc<SlackHyperClient>,
    states: SlackClientEventsUserState,
) -> UserCallbackResult<()> {
    let msg_event = match event.event {
        SlackEventCallbackBody::Message(msg) => msg,
        _ => return Ok(()),
    };

    // Skip bot messages and message edits/deletes
    if msg_event.sender.user.is_none() || msg_event.subtype.is_some() {
        return Ok(());
    }

    let state_guard = states.read().await;
    let adapter_state = state_guard
        .get_user_state::<Arc<SlackAdapterState>>()
        .expect("SlackAdapterState must be in user_state");

    let user_id = msg_event.sender.user.as_ref().map(|u| u.0.clone());
    let team_id = event.team_id.0.clone();
    let channel_id = msg_event
        .origin
        .channel
        .as_ref()
        .map(|c| c.0.clone())
        .unwrap_or_default();
    let ts = msg_event.origin.ts.0.clone();

    // Load permissions snapshot
    let perms = adapter_state.permissions.load();

    // DM filter: Slack DM channel IDs start with "D"
    if channel_id.starts_with('D') {
        if perms.dm_allowed_users.is_empty() {
            return Ok(());
        }
        if let Some(ref uid) = user_id {
            if !perms.dm_allowed_users.contains(uid) {
                return Ok(());
            }
        }
    }

    // Workspace filter
    if let Some(ref filter) = perms.workspace_filter {
        if !filter.contains(&team_id) {
            return Ok(());
        }
    }

    // Channel filter
    if let Some(allowed_channels) = perms.channel_filter.get(&team_id) {
        if !allowed_channels.is_empty() && !allowed_channels.contains(&channel_id) {
            return Ok(());
        }
    }

    // Build conversation ID (threaded conversations get their own channel)
    let conversation_id = if let Some(ref thread_ts) = msg_event.origin.thread_ts {
        format!("slack:{}:{}:{}", team_id, channel_id, thread_ts.0)
    } else {
        format!("slack:{}:{}", team_id, channel_id)
    };

    // Extract content
    let content = if let Some(ref msg_content) = msg_event.content {
        if let Some(ref files) = msg_content.files {
            let attachments: Vec<crate::Attachment> = files
                .iter()
                .filter_map(|f| {
                    let url = f.url_private.as_ref()?;
                    Some(crate::Attachment {
                        filename: f.name.clone().unwrap_or_else(|| "unnamed".into()),
                        mime_type: f.mimetype.as_ref().map(|m| m.0.clone()).unwrap_or_default(),
                        url: url.to_string(),
                        size_bytes: None,
                    })
                })
                .collect();

            if attachments.is_empty() {
                MessageContent::Text(msg_content.text.clone().unwrap_or_default())
            } else {
                MessageContent::Media {
                    text: msg_content.text.clone(),
                    attachments,
                }
            }
        } else {
            MessageContent::Text(msg_content.text.clone().unwrap_or_default())
        }
    } else {
        MessageContent::Text(String::new())
    };

    // Build metadata
    let mut metadata = HashMap::new();
    metadata.insert(
        "slack_workspace_id".into(),
        serde_json::Value::String(team_id),
    );
    metadata.insert(
        "slack_channel_id".into(),
        serde_json::Value::String(channel_id),
    );
    metadata.insert(
        "slack_message_ts".into(),
        serde_json::Value::String(ts.clone()),
    );
    if let Some(ref thread_ts) = msg_event.origin.thread_ts {
        metadata.insert(
            "slack_thread_ts".into(),
            serde_json::Value::String(thread_ts.0.clone()),
        );
    }
    if let Some(ref uid) = user_id {
        metadata.insert(
            "slack_user_id".into(),
            serde_json::Value::String(uid.clone()),
        );
    }

    // Try to get user display name
    if let Some(ref uid) = msg_event.sender.user {
        let token = SlackApiToken::new(SlackApiTokenValue(adapter_state.bot_token.clone()));
        let session = client.open_session(&token);
        if let Ok(user_info) = session
            .users_info(&SlackApiUsersInfoRequest::new(uid.clone()))
            .await
        {
            if let Some(name) = user_info.user.name {
                metadata.insert(
                    "sender_display_name".into(),
                    serde_json::Value::String(name),
                );
            }
        }
    }

    let inbound = InboundMessage {
        id: ts,
        source: "slack".into(),
        conversation_id,
        sender_id: user_id.unwrap_or_default(),
        agent_id: None,
        content,
        timestamp: chrono::Utc::now(),
        metadata,
    };

    if let Err(error) = adapter_state.inbound_tx.send(inbound).await {
        tracing::warn!(%error, "failed to send inbound message from Slack");
    }

    Ok(())
}

fn slack_error_handler(
    err: Box<dyn std::error::Error + Send + Sync>,
    _client: Arc<SlackHyperClient>,
    _states: SlackClientEventsUserState,
) -> HttpStatusCode {
    tracing::warn!(error = %err, "slack socket mode error");
    HttpStatusCode::OK
}

impl Messaging for SlackAdapter {
    fn name(&self) -> &str {
        "slack"
    }

    async fn start(&self) -> crate::Result<InboundStream> {
        let (inbound_tx, inbound_rx) = mpsc::channel(256);
        let (shutdown_tx, mut shutdown_rx) = mpsc::channel::<()>(1);

        *self.shutdown_tx.write().await = Some(shutdown_tx);

        let client = Arc::new(SlackClient::new(
            SlackClientHyperConnector::new().context("failed to create slack connector")?,
        ));

        let adapter_state = Arc::new(SlackAdapterState {
            inbound_tx,
            permissions: self.permissions.clone(),
            bot_token: self.bot_token.clone(),
        });

        let callbacks = SlackSocketModeListenerCallbacks::new().with_push_events(handle_push_event);

        let listener_environment = Arc::new(
            SlackClientEventsListenerEnvironment::new(client.clone())
                .with_error_handler(slack_error_handler)
                .with_user_state(adapter_state),
        );

        let listener = SlackClientSocketModeListener::new(
            &SlackClientSocketModeConfig::new(),
            listener_environment,
            callbacks,
        );

        let app_token = SlackApiToken::new(SlackApiTokenValue(self.app_token.clone()));

        tokio::spawn(async move {
            if let Err(error) = listener.listen_for(&app_token).await {
                tracing::error!(%error, "failed to start slack socket mode listener");
                return;
            }

            tracing::info!("slack socket mode connected");

            tokio::select! {
                exit_code = listener.serve() => {
                    tracing::info!(exit_code, "slack socket mode listener stopped");
                }
                _ = shutdown_rx.recv() => {
                    tracing::info!("slack socket mode shutting down");
                    listener.shutdown().await;
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
        let (client, token) = self.create_session()?;
        let session = client.open_session(&token);

        let channel_id = extract_channel_id(message)?;

        match response {
            OutboundResponse::Text(text) => {
                let thread_ts = extract_thread_ts(message);

                for chunk in split_message(&text, 4000) {
                    let mut req = SlackApiChatPostMessageRequest::new(
                        channel_id.clone(),
                        SlackMessageContent::new().with_text(chunk),
                    );
                    req = req.opt_thread_ts(thread_ts.clone());

                    session
                        .chat_post_message(&req)
                        .await
                        .context("failed to send slack message")?;
                }
            }
            OutboundResponse::ThreadReply {
                thread_name: _,
                text,
            } => {
                // Use existing thread_ts, or create a thread from the source message
                let thread_ts = extract_thread_ts(message).or_else(|| extract_message_ts(message));

                for chunk in split_message(&text, 4000) {
                    let mut req = SlackApiChatPostMessageRequest::new(
                        channel_id.clone(),
                        SlackMessageContent::new().with_text(chunk),
                    );
                    req = req.opt_thread_ts(thread_ts.clone());

                    session
                        .chat_post_message(&req)
                        .await
                        .context("failed to send slack thread reply")?;
                }
            }
            OutboundResponse::File {
                filename,
                data,
                mime_type,
                caption,
            } => {
                // Slack's v2 upload flow: get upload URL, upload bytes, complete
                let upload_url_response = session
                    .get_upload_url_external(&SlackApiFilesGetUploadUrlExternalRequest::new(
                        filename.clone(),
                        data.len(),
                    ))
                    .await
                    .context("failed to get slack upload URL")?;

                session
                    .files_upload_via_url(&SlackApiFilesUploadViaUrlRequest::new(
                        upload_url_response.upload_url,
                        data,
                        mime_type,
                    ))
                    .await
                    .context("failed to upload file to slack")?;

                let thread_ts = extract_thread_ts(message);
                let file_complete =
                    SlackApiFilesComplete::new(upload_url_response.file_id).with_title(filename);

                let mut complete_request =
                    SlackApiFilesCompleteUploadExternalRequest::new(vec![file_complete])
                        .with_channel_id(channel_id.clone());

                complete_request = complete_request.opt_initial_comment(caption);
                complete_request = complete_request.opt_thread_ts(thread_ts);

                session
                    .files_complete_upload_external(&complete_request)
                    .await
                    .context("failed to complete slack file upload")?;
            }
            OutboundResponse::Reaction(emoji) => {
                let ts =
                    extract_message_ts(message).context("missing slack_message_ts for reaction")?;

                let reaction_name = sanitize_reaction_name(&emoji);

                let req = SlackApiReactionsAddRequest::new(
                    channel_id.clone(),
                    SlackReactionName(reaction_name),
                    ts,
                );

                session
                    .reactions_add(&req)
                    .await
                    .context("failed to add slack reaction")?;
            }
            OutboundResponse::StreamStart => {
                let req = SlackApiChatPostMessageRequest::new(
                    channel_id.clone(),
                    SlackMessageContent::new().with_text("\u{200B}".into()),
                );

                let resp = session
                    .chat_post_message(&req)
                    .await
                    .context("failed to send stream placeholder")?;

                self.active_messages
                    .write()
                    .await
                    .insert(message.id.clone(), resp.ts.0);
            }
            OutboundResponse::StreamChunk(text) => {
                let active = self.active_messages.read().await;
                if let Some(ts) = active.get(&message.id) {
                    let display_text = if text.len() > 4000 {
                        let end = text.floor_char_boundary(3997);
                        format!("{}...", &text[..end])
                    } else {
                        text
                    };

                    let req = SlackApiChatUpdateRequest::new(
                        channel_id.clone(),
                        SlackMessageContent::new().with_text(display_text),
                        SlackTs(ts.clone()),
                    );

                    if let Err(error) = session.chat_update(&req).await {
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
        let (client, token) = self.create_session()?;
        let session = client.open_session(&token);
        let channel_id = extract_channel_id(message)?;

        match status {
            StatusUpdate::Thinking => {
                if let Some(ts) = extract_message_ts(message) {
                    let req = SlackApiReactionsAddRequest::new(
                        channel_id,
                        SlackReactionName("thinking_face".into()),
                        ts,
                    );
                    // Best-effort, don't fail the whole response
                    if let Err(error) = session.reactions_add(&req).await {
                        tracing::debug!(%error, "failed to add thinking reaction");
                    }
                }
            }
            _ => {
                if let Some(ts) = extract_message_ts(message) {
                    let req = SlackApiReactionsRemoveRequest::new(SlackReactionName(
                        "thinking_face".into(),
                    ))
                    .with_channel(channel_id)
                    .with_timestamp(ts);

                    let _ = session.reactions_remove(&req).await;
                }
            }
        }

        Ok(())
    }

    async fn broadcast(&self, target: &str, response: OutboundResponse) -> crate::Result<()> {
        let (client, token) = self.create_session()?;
        let session = client.open_session(&token);
        let channel_id = SlackChannelId(target.to_string());

        if let OutboundResponse::Text(text) = response {
            for chunk in split_message(&text, 4000) {
                let req = SlackApiChatPostMessageRequest::new(
                    channel_id.clone(),
                    SlackMessageContent::new().with_text(chunk),
                );

                session
                    .chat_post_message(&req)
                    .await
                    .context("failed to broadcast slack message")?;
            }
        }

        Ok(())
    }

    async fn fetch_history(
        &self,
        message: &InboundMessage,
        limit: usize,
    ) -> crate::Result<Vec<HistoryMessage>> {
        let (client, token) = self.create_session()?;
        let session = client.open_session(&token);
        let channel_id = extract_channel_id(message)?;
        let thread_ts = extract_thread_ts(message);
        let capped_limit = limit.min(100) as u16;

        let messages = if let Some(ts) = thread_ts {
            let req = SlackApiConversationsRepliesRequest::new(channel_id.clone(), ts)
                .with_limit(capped_limit);

            session
                .conversations_replies(&req)
                .await
                .context("failed to fetch slack thread history")?
                .messages
        } else {
            let req = SlackApiConversationsHistoryRequest::new()
                .with_channel(channel_id.clone())
                .with_limit(capped_limit);

            session
                .conversations_history(&req)
                .await
                .context("failed to fetch slack channel history")?
                .messages
        };

        // Slack returns newest-first, reverse to chronological
        let result: Vec<HistoryMessage> = messages
            .into_iter()
            .rev()
            .map(|msg| {
                let user_id = msg.sender.user.as_ref().map(|u| u.0.clone());
                let is_bot = user_id.is_none() || msg.sender.bot_id.is_some();
                let author = user_id.unwrap_or_else(|| "bot".into());

                HistoryMessage {
                    author,
                    content: msg.content.text.clone().unwrap_or_default(),
                    is_bot,
                }
            })
            .collect();

        tracing::info!(
            count = result.len(),
            channel_id = %channel_id.0,
            "fetched slack message history"
        );

        Ok(result)
    }

    async fn health_check(&self) -> crate::Result<()> {
        let (client, token) = self.create_session()?;
        let session = client.open_session(&token);

        session
            .api_test(&SlackApiTestRequest::new())
            .await
            .context("slack health check failed")?;

        Ok(())
    }

    async fn shutdown(&self) -> crate::Result<()> {
        self.active_messages.write().await.clear();

        if let Some(tx) = self.shutdown_tx.write().await.take() {
            let _ = tx.send(()).await;
        }

        tracing::info!("slack adapter shut down");
        Ok(())
    }
}

// -- Helper functions --

fn extract_channel_id(message: &InboundMessage) -> anyhow::Result<SlackChannelId> {
    message
        .metadata
        .get("slack_channel_id")
        .and_then(|v| v.as_str())
        .map(|s| SlackChannelId(s.to_string()))
        .context("missing slack_channel_id in metadata")
}

fn extract_message_ts(message: &InboundMessage) -> Option<SlackTs> {
    message
        .metadata
        .get("slack_message_ts")
        .and_then(|v| v.as_str())
        .map(|s| SlackTs(s.to_string()))
}

fn extract_thread_ts(message: &InboundMessage) -> Option<SlackTs> {
    message
        .metadata
        .get("slack_thread_ts")
        .and_then(|v| v.as_str())
        .map(|s| SlackTs(s.to_string()))
}

/// Split a message into chunks that fit within Slack's character limit.
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

/// Sanitize an emoji string for Slack reactions (remove colons, lowercase).
fn sanitize_reaction_name(emoji: &str) -> String {
    emoji
        .trim()
        .trim_start_matches(':')
        .trim_end_matches(':')
        .to_lowercase()
}
