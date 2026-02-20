//! Slack messaging adapter using slack-morphism.
//!
//! ## Features
//!
//! **Inbound**
//! - Plain text and file-attachment messages (Socket Mode)
//! - `app_mention` events — agent responds when @-mentioned in any channel
//! - Message subtype filtering (edits/deletes ignored)
//! - Per-workspace / per-channel / DM permission filtering (hot-reloadable)
//! - Full user identity resolution (display name, mention tag)
//!
//! **Outbound**
//! - Plain text with smart UTF-8-safe chunking
//! - Thread replies
//! - File uploads (v2 flow)
//! - Emoji reactions (add + remove)
//! - Ephemeral messages (visible only to the triggering user)
//! - Block Kit rich messages with plain-text fallback
//! - Scheduled messages (`chat.scheduleMessage`)
//! - Streaming via `chat.update` edits
//! - Typing indicator via `assistant.threads.setStatus`
//! - DM broadcast via `conversations.open`

use crate::config::{SlackCommandConfig, SlackPermissions};
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
    bot_user_id: String,
    /// Maps slash command string (e.g. `"/ask"`) → agent_id.
    /// Built once at start() from the config; read-only afterwards.
    commands: Arc<HashMap<String, String>>,
    /// Cache of resolved user identities to avoid repeated `users.info` API calls.
    user_identity_cache: Arc<RwLock<HashMap<String, SlackUserIdentity>>>,
    /// Cache of resolved channel names to avoid repeated `conversations.info` API calls.
    channel_name_cache: Arc<RwLock<HashMap<String, String>>>,
}

#[derive(Debug, Clone)]
struct SlackUserIdentity {
    display_name: String,
    username: Option<String>,
}

/// Slack adapter.
pub struct SlackAdapter {
    bot_token: String,
    app_token: String,
    permissions: Arc<ArcSwap<SlackPermissions>>,
    /// Shared HTTP client — constructed once, reused across all API calls.
    /// Holds a hyper connection pool internally; allocating one per call would
    /// discard that pool on every status update / respond / broadcast.
    client: Arc<SlackHyperClient>,
    /// Pre-built API token wrapping `bot_token`. Created once alongside `client`.
    token: SlackApiToken,
    /// Maps InboundMessage.id → Slack ts for streaming edits.
    active_messages: Arc<RwLock<HashMap<String, String>>>,
    shutdown_tx: Arc<RwLock<Option<mpsc::Sender<()>>>>,
    /// Slash command routing: command string → agent_id.
    commands: Arc<HashMap<String, String>>,
}

impl SlackAdapter {
    pub fn new(
        bot_token: impl Into<String>,
        app_token: impl Into<String>,
        permissions: Arc<ArcSwap<SlackPermissions>>,
        commands: Vec<SlackCommandConfig>,
    ) -> anyhow::Result<Self> {
        let bot_token = bot_token.into();
        let client = Arc::new(SlackClient::new(
            SlackClientHyperConnector::new().context("failed to create slack HTTP connector")?,
        ));
        let token = SlackApiToken::new(SlackApiTokenValue(bot_token.clone()));
        let commands_map: HashMap<String, String> = commands
            .into_iter()
            .map(|c| (c.command, c.agent_id))
            .collect();
        Ok(Self {
            bot_token,
            app_token: app_token.into(),
            permissions,
            client,
            token,
            active_messages: Arc::new(RwLock::new(HashMap::new())),
            shutdown_tx: Arc::new(RwLock::new(None)),
            commands: Arc::new(commands_map),
        })
    }

    /// Open a session against the cached client using the cached bot token.
    fn session(&self) -> SlackClientSession<'_, SlackClientHyperHttpsConnector> {
        self.client.open_session(&self.token)
    }
}

// ---------------------------------------------------------------------------
// Inbound event handlers (fn pointers — slack-morphism requirement)
// ---------------------------------------------------------------------------

/// Handle regular channel/DM messages.
async fn handle_push_event(
    event: SlackPushEventCallback,
    client: Arc<SlackHyperClient>,
    states: SlackClientEventsUserState,
) -> UserCallbackResult<()> {
    match event.event {
        SlackEventCallbackBody::Message(msg) => {
            handle_message_event(msg, &event.team_id, client, states).await
        }
        SlackEventCallbackBody::AppMention(mention) => {
            handle_app_mention_event(mention, &event.team_id, client, states).await
        }
        _ => Ok(()),
    }
}

/// Core logic shared by Message and AppMention handlers.
async fn handle_message_event(
    msg_event: SlackMessageEvent,
    team_id: &SlackTeamId,
    client: Arc<SlackHyperClient>,
    states: SlackClientEventsUserState,
) -> UserCallbackResult<()> {
    // Skip message edits / deletes / bot_message subtypes
    if msg_event.subtype.is_some() {
        return Ok(());
    }

    let state_guard = states.read().await;
    let adapter_state = state_guard
        .get_user_state::<Arc<SlackAdapterState>>()
        .expect("SlackAdapterState must be in user_state");

    let user_id = msg_event.sender.user.as_ref().map(|u| u.0.clone());

    if user_id.as_deref() == Some(&adapter_state.bot_user_id) {
        return Ok(()); // ignore self
    }
    if user_id.is_none() {
        return Ok(()); // system message
    }

    let team_id_str = team_id.0.clone();
    let channel_id = msg_event
        .origin
        .channel
        .as_ref()
        .map(|c| c.0.clone())
        .unwrap_or_default();
    let ts = msg_event.origin.ts.0.clone();

    let perms = adapter_state.permissions.load();

    // DM filter
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
        if !filter.contains(&team_id_str) {
            return Ok(());
        }
    }

    // Channel filter
    if let Some(allowed) = perms.channel_filter.get(&team_id_str) {
        if !allowed.is_empty() && !allowed.contains(&channel_id) {
            return Ok(());
        }
    }

    let conversation_id = if let Some(ref thread_ts) = msg_event.origin.thread_ts {
        format!("slack:{}:{}:{}", team_id_str, channel_id, thread_ts.0)
    } else {
        format!("slack:{}:{}", team_id_str, channel_id)
    };

    let content = extract_message_content(&msg_event.content);

    let (metadata, formatted_author) = build_metadata_and_author(
        &team_id_str,
        &channel_id,
        &ts,
        msg_event.origin.thread_ts.as_ref().map(|t| t.0.as_str()),
        user_id.as_deref(),
        msg_event.sender.user.as_ref(),
        &client,
        &adapter_state.bot_token,
        &adapter_state.user_identity_cache,
        &adapter_state.channel_name_cache,
    )
    .await;

    send_inbound(
        &adapter_state.inbound_tx,
        ts,
        conversation_id,
        user_id.unwrap_or_default(),
        content,
        metadata,
        formatted_author,
    )
    .await;

    Ok(())
}

/// Handle `app_mention` events — fired when the bot is @-mentioned in a channel
/// it may not be a primary member of.
///
/// `SlackAppMentionEvent` has a flat `user: SlackUserId` field (not a `sender` sub-struct)
/// and a flat `channel: SlackChannelId` field (not nested in `origin`).
async fn handle_app_mention_event(
    mention: SlackAppMentionEvent,
    team_id: &SlackTeamId,
    client: Arc<SlackHyperClient>,
    states: SlackClientEventsUserState,
) -> UserCallbackResult<()> {
    let state_guard = states.read().await;
    let adapter_state = state_guard
        .get_user_state::<Arc<SlackAdapterState>>()
        .expect("SlackAdapterState must be in user_state");

    let user_id = mention.user.0.clone();

    if user_id == adapter_state.bot_user_id {
        return Ok(());
    }

    let team_id_str = team_id.0.clone();
    let channel_id = mention.channel.0.clone();
    let ts = mention.origin.ts.0.clone();

    let perms = adapter_state.permissions.load();

    // Workspace filter applies to mentions too
    if let Some(ref filter) = perms.workspace_filter {
        if !filter.contains(&team_id_str) {
            return Ok(());
        }
    }

    // Channel filter — same logic as handle_message_event
    if let Some(allowed) = perms.channel_filter.get(&team_id_str) {
        if !allowed.is_empty() && !allowed.contains(&channel_id) {
            return Ok(());
        }
    }

    let conversation_id = if let Some(ref thread_ts) = mention.origin.thread_ts {
        format!("slack:{}:{}:{}", team_id_str, channel_id, thread_ts.0)
    } else {
        format!("slack:{}:{}", team_id_str, channel_id)
    };

    // Strip the leading @-mention from the text so the agent sees clean input
    let raw_text = mention.content.text.clone().unwrap_or_default();
    let text = strip_bot_mention(&raw_text, &adapter_state.bot_user_id);
    let content = MessageContent::Text(text);

    let slack_uid = SlackUserId(user_id.clone());
    let (metadata, formatted_author) = build_metadata_and_author(
        &team_id_str,
        &channel_id,
        &ts,
        mention.origin.thread_ts.as_ref().map(|t| t.0.as_str()),
        Some(&user_id),
        Some(&slack_uid),
        &client,
        &adapter_state.bot_token,
        &adapter_state.user_identity_cache,
        &adapter_state.channel_name_cache,
    )
    .await;

    send_inbound(
        &adapter_state.inbound_tx,
        ts,
        conversation_id,
        user_id,
        content,
        metadata,
        formatted_author,
    )
    .await;

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

/// Handle Slack slash command events (e.g. `/ask What is the weather?`).
///
/// Slack requires an acknowledgement within 3 seconds. This handler acks
/// immediately with an empty 200 and dispatches the command as an `InboundMessage`
/// asynchronously. The agent's reply arrives via the normal `respond()` path.
///
/// Commands not listed in the config are acknowledged but produce a brief
/// "not configured" reply so the user gets feedback instead of silence.
///
/// Workspace and channel permission filters are applied identically to how
/// regular messages are filtered — a command from an unauthorized workspace
/// or channel is silently dropped (Slack does not expect an error response
/// for permission denials, only for unhandled commands).
async fn handle_command_event(
    event: SlackCommandEvent,
    _client: Arc<SlackHyperClient>,
    states: SlackClientEventsUserState,
) -> UserCallbackResult<SlackCommandEventResponse> {
    let state_guard = states.read().await;
    let adapter_state = state_guard
        .get_user_state::<Arc<SlackAdapterState>>()
        .expect("SlackAdapterState must be in user_state");

    let command_str = event.command.0.clone();
    let team_id = event.team_id.0.clone();
    let channel_id = event.channel_id.0.clone();
    let user_id = event.user_id.0.clone();
    let msg_id = event.trigger_id.0.clone();
    let text = event.text.clone().unwrap_or_default();

    // Apply the same workspace / channel permission filters as regular messages.
    // An unauthorized command is silently acked with no reply — same as a message
    // from an unauthorized channel being dropped.
    {
        let perms = adapter_state.permissions.load();

        if let Some(ref filter) = perms.workspace_filter {
            if !filter.contains(&team_id) {
                tracing::debug!(
                    team_id = %team_id,
                    command = %command_str,
                    "slash command from unauthorized workspace — dropping"
                );
                return Ok(SlackCommandEventResponse {
                    content: SlackMessageContent::new(),
                    response_type: Some(SlackMessageResponseType::Ephemeral),
                });
            }
        }

        if let Some(allowed) = perms.channel_filter.get(&team_id) {
            if !allowed.is_empty() && !allowed.contains(&channel_id) {
                tracing::debug!(
                    channel_id = %channel_id,
                    command = %command_str,
                    "slash command from unauthorized channel — dropping"
                );
                return Ok(SlackCommandEventResponse {
                    content: SlackMessageContent::new(),
                    response_type: Some(SlackMessageResponseType::Ephemeral),
                });
            }
        }
    }

    if !adapter_state.commands.contains_key(&command_str) {
        tracing::warn!(
            command = %command_str,
            user_id = %user_id,
            "slash command not configured — ignoring"
        );
        return Ok(SlackCommandEventResponse {
            content: SlackMessageContent::new().with_text(format!(
                "`{}` is not configured on this Spacebot instance.",
                command_str
            )),
            response_type: Some(SlackMessageResponseType::Ephemeral),
        });
    }

    let agent_id = adapter_state.commands[&command_str].clone();

    let conversation_id = format!("slack:{}:{}", team_id, channel_id);

    let mut metadata = HashMap::new();
    metadata.insert(
        "slack_workspace_id".into(),
        serde_json::Value::String(team_id.clone()),
    );
    metadata.insert(
        "slack_channel_id".into(),
        serde_json::Value::String(channel_id.clone()),
    );
    metadata.insert(
        "slack_user_id".into(),
        serde_json::Value::String(user_id.clone()),
    );
    metadata.insert(
        "sender_id".into(),
        serde_json::Value::String(user_id.clone()),
    );
    metadata.insert(
        "slack_command".into(),
        serde_json::Value::String(command_str.clone()),
    );
    metadata.insert(
        "slack_user_mention".into(),
        serde_json::Value::String(format!("<@{}>", user_id)),
    );
    // Embed the agent_id hint so the router can honour command-specific routing
    // without requiring a separate binding entry per command.
    metadata.insert(
        "slack_command_agent_id".into(),
        serde_json::Value::String(agent_id),
    );

    let content = MessageContent::Text(format!("{} {}", command_str, text).trim().to_string());

    let inbound = InboundMessage {
        id: msg_id,
        source: "slack".into(),
        conversation_id,
        sender_id: user_id.clone(),
        agent_id: None,
        content,
        timestamp: chrono::Utc::now(),
        metadata,
        formatted_author: Some(format!("<@{}>", user_id)),
    };

    if let Err(error) = adapter_state.inbound_tx.send(inbound).await {
        tracing::warn!(%error, "failed to enqueue slash command as inbound message");
    }

    // Ack immediately with an empty body — the real reply comes via respond().
    Ok(SlackCommandEventResponse {
        content: SlackMessageContent::new(),
        response_type: Some(SlackMessageResponseType::Ephemeral),
    })
}

/// Handle Slack Block Kit interaction events (button clicks, select menus, etc.).
///
/// Only `block_actions` is turned into an `InboundMessage`; other interaction
/// types (view submissions, shortcuts, etc.) are logged and acknowledged.
async fn handle_interaction_event(
    event: SlackInteractionEvent,
    _client: Arc<SlackHyperClient>,
    states: SlackClientEventsUserState,
) -> UserCallbackResult<()> {
    let SlackInteractionEvent::BlockActions(block_actions) = event else {
        // Acknowledge non-block-action interactions without processing.
        tracing::debug!("received non-block-action interaction event — ignoring");
        return Ok(());
    };

    let state_guard = states.read().await;
    let adapter_state = state_guard
        .get_user_state::<Arc<SlackAdapterState>>()
        .expect("SlackAdapterState must be in user_state");

    let user_id = block_actions
        .user
        .as_ref()
        .map(|u| u.id.0.clone())
        .unwrap_or_default();

    let team_id = block_actions.team.id.0.clone();

    let channel_id = block_actions
        .channel
        .as_ref()
        .map(|c| c.id.0.clone())
        .unwrap_or_default();

    // Apply workspace / channel permission filters — interactions are subject to
    // the same access rules as regular messages.
    {
        let perms = adapter_state.permissions.load();

        if let Some(ref filter) = perms.workspace_filter {
            if !filter.contains(&team_id) {
                tracing::debug!(
                    team_id = %team_id,
                    "block_actions interaction from unauthorized workspace — dropping"
                );
                return Ok(());
            }
        }

        if !channel_id.is_empty() {
            if let Some(allowed) = perms.channel_filter.get(&team_id) {
                if !allowed.is_empty() && !allowed.contains(&channel_id) {
                    tracing::debug!(
                        channel_id = %channel_id,
                        "block_actions interaction from unauthorized channel — dropping"
                    );
                    return Ok(());
                }
            }
        }
    }

    let message_ts = match &block_actions.container {
        SlackInteractionActionContainer::Message(msg_container) => {
            Some(msg_container.message_ts.0.clone())
        }
        _ => None,
    };

    // Use trigger_id as the unique message id for this interaction turn.
    let msg_id = block_actions.trigger_id.0.clone();

    let conversation_id = if let Some(ref ts) = message_ts {
        format!("slack:{}:{}:{}", team_id, channel_id, ts)
    } else {
        format!("slack:{}:{}", team_id, channel_id)
    };

    // Process each action in the payload as a separate inbound message.
    // In practice Slack sends one action per interaction, but the API allows many.
    let actions = block_actions.actions.unwrap_or_default();

    if actions.is_empty() {
        tracing::debug!("block_actions interaction had no actions — ignoring");
        return Ok(());
    }

    for (idx, action) in actions.iter().enumerate() {
        let action_id = action.action_id.0.clone();
        let block_id = action.block_id.as_ref().map(|b| b.0.clone());
        let value = action.value.clone();
        let label = action.selected_option.as_ref().and_then(|o| match &o.text {
            SlackBlockText::Plain(pt) => Some(pt.text.clone()),
            SlackBlockText::MarkDown(md) => Some(md.text.clone()),
        });

        let content = MessageContent::Interaction {
            action_id: action_id.clone(),
            block_id: block_id.clone(),
            values: value.map(|v| vec![v]).unwrap_or_default(),
            label: label.clone(),
            message_ts: message_ts.clone(),
        };

        // Use trigger_id for the first action, trigger_id:index for subsequent ones.
        let id = if idx == 0 {
            msg_id.clone()
        } else {
            format!("{}:{}", msg_id, idx)
        };

        let mut metadata = HashMap::new();
        metadata.insert(
            "slack_workspace_id".into(),
            serde_json::Value::String(team_id.clone()),
        );
        metadata.insert(
            "slack_channel_id".into(),
            serde_json::Value::String(channel_id.clone()),
        );
        metadata.insert(
            "slack_user_id".into(),
            serde_json::Value::String(user_id.clone()),
        );
        metadata.insert(
            "sender_id".into(),
            serde_json::Value::String(user_id.clone()),
        );
        metadata.insert(
            "slack_user_mention".into(),
            serde_json::Value::String(format!("<@{}>", user_id)),
        );
        if let Some(ref ts) = message_ts {
            metadata.insert(
                "slack_thread_ts".into(),
                serde_json::Value::String(ts.clone()),
            );
            metadata.insert(
                "slack_message_ts".into(),
                serde_json::Value::String(ts.clone()),
            );
        }
        metadata.insert(
            "slack_action_id".into(),
            serde_json::Value::String(action_id),
        );
        if let Some(ref bid) = block_id {
            metadata.insert(
                "slack_block_id".into(),
                serde_json::Value::String(bid.clone()),
            );
        }

        let inbound = InboundMessage {
            id,
            source: "slack".into(),
            conversation_id: conversation_id.clone(),
            sender_id: user_id.clone(),
            agent_id: None,
            content,
            timestamp: chrono::Utc::now(),
            metadata,
            formatted_author: Some(format!("<@{}>", user_id)),
        };

        if let Err(error) = adapter_state.inbound_tx.send(inbound).await {
            tracing::warn!(%error, "failed to enqueue block interaction as inbound message");
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Messaging trait impl
// ---------------------------------------------------------------------------

impl Messaging for SlackAdapter {
    fn name(&self) -> &str {
        "slack"
    }

    async fn start(&self) -> crate::Result<InboundStream> {
        let (inbound_tx, inbound_rx) = mpsc::channel(256);
        let (shutdown_tx, mut shutdown_rx) = mpsc::channel::<()>(1);

        *self.shutdown_tx.write().await = Some(shutdown_tx);

        // Reuse the shared client for auth.test; no new allocation needed.
        let auth_response = self
            .session()
            .auth_test()
            .await
            .context("failed to call auth.test for bot user ID")?;
        let bot_user_id = auth_response.user_id.0.clone();
        tracing::info!(bot_user_id = %bot_user_id, "slack bot user ID resolved");

        let adapter_state = Arc::new(SlackAdapterState {
            inbound_tx,
            permissions: self.permissions.clone(),
            bot_token: self.bot_token.clone(),
            bot_user_id,
            commands: self.commands.clone(),
            user_identity_cache: Arc::new(RwLock::new(HashMap::new())),
            channel_name_cache: Arc::new(RwLock::new(HashMap::new())),
        });

        let callbacks = SlackSocketModeListenerCallbacks::new()
            .with_push_events(handle_push_event)
            .with_command_events(handle_command_event)
            .with_interaction_events(handle_interaction_event);

        // The socket mode listener needs its own client instance — it manages
        // a persistent WebSocket connection internally and owns that client for
        // the lifetime of the connection. The shared `self.client` is for REST calls.
        let listener_client = Arc::new(SlackClient::new(
            SlackClientHyperConnector::new()
                .context("failed to create slack socket mode connector")?,
        ));

        // The socket mode listener needs its own client — it owns a persistent
        // WebSocket connection. The shared self.client is for REST calls only.
        let listener_client = Arc::new(SlackClient::new(
            SlackClientHyperConnector::new()
                .context("failed to create slack socket mode connector")?,
        ));

        let listener_environment = Arc::new(
            SlackClientEventsListenerEnvironment::new(listener_client.clone())
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

        Ok(Box::pin(tokio_stream::wrappers::ReceiverStream::new(
            inbound_rx,
        )))
    }

    /// Show a typing-style status in Slack Assistant threads while the agent is thinking.
    ///
    /// Uses `assistant.threads.setStatus` — the correct API for Socket Mode bots running
    /// in Slack Assistant thread contexts.
    ///
    /// **Scope limitation (Slack API):** this API only works inside Slack Assistant threads
    /// (i.e. messages that carry a `thread_ts`). Regular channel messages and DMs do not
    /// support `setStatus`; this function no-ops for them rather than erroring. If you are
    /// not seeing typing indicators, verify the conversation is inside an Assistant thread.
    ///
    /// Pass an empty string to clear the status (e.g. on `StopTyping`).
    async fn send_status(
        &self,
        message: &InboundMessage,
        status: StatusUpdate,
    ) -> crate::Result<()> {
        let thread_ts = match extract_thread_ts(message) {
            Some(ts) => ts,
            None => {
                tracing::debug!(
                    message_id = %message.id,
                    "skipping assistant.threads.setStatus — message has no thread_ts \
                     (typing indicators only work in Slack Assistant threads)"
                );
                return Ok(());
            }
        };
        let channel_id = match extract_channel_id(message) {
            Ok(id) => id,
            Err(_) => return Ok(()),
        };

        let status_text = match &status {
            StatusUpdate::Thinking => "Thinking…".to_string(),
            StatusUpdate::StopTyping => String::new(), // empty string clears the status
            StatusUpdate::ToolStarted { .. } => "Working…".to_string(),
            StatusUpdate::ToolCompleted { .. } => "Working…".to_string(),
            _ => "Working…".to_string(),
        };

        let session = self.session();

        let req = SlackApiAssistantThreadsSetStatusRequest {
            channel_id,
            thread_ts,
            status: status_text,
        };

        // Best-effort — don't propagate status errors into the main response pipeline.
        if let Err(err) = session.assistant_threads_set_status(&req).await {
            tracing::debug!(error = %err, "failed to set slack assistant thread status (non-fatal)");
        }

        Ok(())
    }

    async fn respond(
        &self,
        message: &InboundMessage,
        response: OutboundResponse,
    ) -> crate::Result<()> {
        let session = self.session();
        let channel_id = extract_channel_id(message)?;

        match response {
            OutboundResponse::Text(text) => {
                let thread_ts = extract_thread_ts(message);

                for chunk in split_message(&text, 12_000) {
                    let mut req = SlackApiChatPostMessageRequest::new(
                        channel_id.clone(),
                        markdown_content(chunk),
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
                let thread_ts = extract_thread_ts(message).or_else(|| extract_message_ts(message));

                for chunk in split_message(&text, 12_000) {
                    let mut req = SlackApiChatPostMessageRequest::new(
                        channel_id.clone(),
                        markdown_content(chunk),
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
                let req = SlackApiReactionsAddRequest::new(
                    channel_id.clone(),
                    SlackReactionName(sanitize_reaction_name(&emoji)),
                    ts,
                );
                session
                    .reactions_add(&req)
                    .await
                    .context("failed to add slack reaction")?;
            }

            OutboundResponse::RemoveReaction(emoji) => {
                let ts = extract_message_ts(message)
                    .context("missing slack_message_ts for reaction removal")?;
                // channel and timestamp are Optional on the remove request
                let req = SlackApiReactionsRemoveRequest::new(SlackReactionName(
                    sanitize_reaction_name(&emoji),
                ))
                .with_channel(channel_id.clone())
                .with_timestamp(ts);
                session
                    .reactions_remove(&req)
                    .await
                    .context("failed to remove slack reaction")?;
            }

            OutboundResponse::Ephemeral { text, user_id } => {
                let thread_ts = extract_thread_ts(message);
                let req = SlackApiChatPostEphemeralRequest::new(
                    channel_id.clone(),
                    SlackUserId(user_id),
                    SlackMessageContent::new().with_text(text),
                )
                .opt_thread_ts(thread_ts);
                session
                    .chat_post_ephemeral(&req)
                    .await
                    .context("failed to send slack ephemeral message")?;
            }

            OutboundResponse::RichMessage { text, blocks, .. } => {
                let thread_ts = extract_thread_ts(message);
                let attempted = blocks.len();
                let slack_blocks = deserialize_blocks(&blocks);
                let dropped = attempted - slack_blocks.len();
                let content = if slack_blocks.is_empty() {
                    if attempted > 0 {
                        tracing::warn!(
                            attempted,
                            "all {} block(s) failed to deserialise — sending plain text fallback",
                            attempted
                        );
                    }
                    SlackMessageContent::new().with_text(text)
                } else {
                    if dropped > 0 {
                        tracing::warn!(
                            dropped,
                            attempted,
                            "{} of {} block(s) dropped due to deserialisation errors",
                            dropped,
                            attempted
                        );
                    }
                    SlackMessageContent::new()
                        .with_text(text)
                        .with_blocks(slack_blocks)
                };
                let mut req = SlackApiChatPostMessageRequest::new(channel_id.clone(), content);
                req = req.opt_thread_ts(thread_ts);
                session
                    .chat_post_message(&req)
                    .await
                    .context("failed to send slack rich message")?;
            }

            OutboundResponse::ScheduledMessage { text, post_at } => {
                let thread_ts = extract_thread_ts(message);
                let post_at_dt = chrono::DateTime::<chrono::Utc>::from_timestamp(post_at, 0)
                    .context("invalid post_at unix timestamp for scheduled message")?;
                let req = SlackApiChatScheduleMessageRequest::new(
                    channel_id.clone(),
                    SlackMessageContent::new().with_text(text),
                    SlackDateTime(post_at_dt),
                )
                .opt_thread_ts(thread_ts);
                session
                    .chat_schedule_message(&req)
                    .await
                    .context("failed to schedule slack message")?;
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
                    let display_text = if text.len() > 12_000 {
                        let end = text.floor_char_boundary(11_997);
                        format!("{}...", &text[..end])
                    } else {
                        text
                    };
                    let req = SlackApiChatUpdateRequest::new(
                        channel_id.clone(),
                        markdown_content(display_text),
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

            OutboundResponse::Status(_) => {
                // Status updates are handled via send_status(); ignored here.
            }
        }

        Ok(())
    }

    async fn broadcast(&self, target: &str, response: OutboundResponse) -> crate::Result<()> {
        let session = self.session();

        let channel_id = if let Some(user_id_str) = target.strip_prefix("dm:") {
            let open_req = SlackApiConversationsOpenRequest::new()
                .with_users(vec![SlackUserId(user_id_str.to_string())]);
            let open_resp = session
                .conversations_open(&open_req)
                .await
                .context("failed to open Slack DM conversation")?;
            open_resp.channel.id
        } else {
            SlackChannelId(target.to_string())
        };

        match response {
            OutboundResponse::Text(text) => {
                for chunk in split_message(&text, 12_000) {
                    let req = SlackApiChatPostMessageRequest::new(
                        channel_id.clone(),
                        markdown_content(chunk),
                    );
                    session
                        .chat_post_message(&req)
                        .await
                        .context("failed to broadcast slack message")?;
                }
            }
            OutboundResponse::RichMessage { text, blocks, .. } => {
                let slack_blocks = deserialize_blocks(&blocks);
                let content = if slack_blocks.is_empty() {
                    SlackMessageContent::new().with_text(text)
                } else {
                    SlackMessageContent::new()
                        .with_text(text)
                        .with_blocks(slack_blocks)
                };
                let req = SlackApiChatPostMessageRequest::new(channel_id.clone(), content);
                session
                    .chat_post_message(&req)
                    .await
                    .context("failed to broadcast slack rich message")?;
            }
            // Other variants are not meaningful for broadcast (e.g. Ephemeral requires a
            // specific user_id from a live conversation, Reaction requires an existing ts,
            // Scheduled/Stream are respond()-only flows).
            other => {
                tracing::warn!(
                    variant = %variant_name(&other),
                    target = %target,
                    "broadcast() received a variant that is not supported for broadcast — ignoring"
                );
            }
        }

        Ok(())
    }

    async fn fetch_history(
        &self,
        message: &InboundMessage,
        limit: usize,
    ) -> crate::Result<Vec<HistoryMessage>> {
        let session = self.session();
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

        let mut user_identity_by_id = HashMap::new();
        for user_id in messages
            .iter()
            .filter_map(|msg| msg.sender.user.as_ref().map(|u| u.0.clone()))
        {
            if user_identity_by_id.contains_key(&user_id) {
                continue;
            }
            if let Ok(user_info) = session
                .users_info(&SlackApiUsersInfoRequest::new(SlackUserId(user_id.clone())))
                .await
            {
                let identity = resolve_slack_user_identity(&user_info.user, &user_id);
                user_identity_by_id.insert(user_id, identity);
            }
        }

        // Slack returns newest-first; reverse to chronological.
        let result: Vec<HistoryMessage> = messages
            .into_iter()
            .rev()
            .map(|msg| {
                let user_id = msg.sender.user.as_ref().map(|u| u.0.clone());
                let is_bot = user_id.is_none() || msg.sender.bot_id.is_some();
                let author = if is_bot {
                    "bot".to_string()
                } else if let Some(uid) = user_id {
                    user_identity_by_id
                        .get(&uid)
                        .map(|i| i.display_name.clone())
                        .unwrap_or_else(|| uid.clone())
                } else {
                    "unknown".to_string()
                };
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
        let session = self.session();
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

// ---------------------------------------------------------------------------
// Helper functions
// ---------------------------------------------------------------------------

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

/// Build a `SlackMessageContent` using a Markdown block with plain text fallback.
///
/// The Markdown block supports standard markdown (bold, italic, lists, code,
/// headings, quotes, links) natively — no mrkdwn conversion needed. The `text`
/// field is set as fallback for notifications and accessibility.
///
/// Cumulative limit for all markdown blocks in a payload is 12,000 characters.
/// For content exceeding 12,000 chars we fall back to plain text to avoid
/// Slack rejecting the payload.
fn markdown_content(text: impl Into<String>) -> SlackMessageContent {
    let text = text.into();
    if text.len() <= 12_000 {
        let block = SlackBlock::Markdown(SlackMarkdownBlock::new(text.clone()));
        SlackMessageContent::new()
            .with_text(text)
            .with_blocks(vec![block])
    } else {
        // Exceeds markdown block limit — send as plain text
        SlackMessageContent::new().with_text(text)
    }
}

/// Extract `MessageContent` from an optional `SlackMessageContent`.
fn extract_message_content(content: &Option<SlackMessageContent>) -> MessageContent {
    let Some(msg_content) = content else {
        return MessageContent::Text(String::new());
    };

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

        if !attachments.is_empty() {
            return MessageContent::Media {
                text: msg_content.text.clone(),
                attachments,
            };
        }
    }

    MessageContent::Text(msg_content.text.clone().unwrap_or_default())
}

/// Build the metadata map and formatted author string shared by all inbound paths.
#[allow(clippy::too_many_arguments)]
async fn build_metadata_and_author(
    team_id: &str,
    channel_id: &str,
    ts: &str,
    thread_ts: Option<&str>,
    user_id: Option<&str>,
    slack_user_id: Option<&SlackUserId>,
    client: &Arc<SlackHyperClient>,
    bot_token: &str,
    user_identity_cache: &Arc<RwLock<HashMap<String, SlackUserIdentity>>>,
    channel_name_cache: &Arc<RwLock<HashMap<String, String>>>,
) -> (HashMap<String, serde_json::Value>, Option<String>) {
    let mut metadata = HashMap::new();

    metadata.insert(
        "slack_workspace_id".into(),
        serde_json::Value::String(team_id.into()),
    );
    metadata.insert(
        "slack_channel_id".into(),
        serde_json::Value::String(channel_id.into()),
    );
    metadata.insert(
        "slack_message_ts".into(),
        serde_json::Value::String(ts.into()),
    );

    if let Some(tts) = thread_ts {
        metadata.insert(
            "slack_thread_ts".into(),
            serde_json::Value::String(tts.into()),
        );
    }

    if let Some(uid) = user_id {
        metadata.insert(
            "slack_user_id".into(),
            serde_json::Value::String(uid.into()),
        );
        metadata.insert("sender_id".into(), serde_json::Value::String(uid.into()));
        metadata.insert(
            "slack_user_mention".into(),
            serde_json::Value::String(format!("<@{uid}>")),
        );
    }

    let token = SlackApiToken::new(SlackApiTokenValue(bot_token.to_string()));
    let session = client.open_session(&token);

    // Resolve channel name via cache or conversations.info API.
    if let Some(name) = channel_name_cache.read().await.get(channel_id).cloned() {
        metadata.insert(
            "slack_channel_name".into(),
            serde_json::Value::String(name),
        );
    } else {
        match session
            .conversations_info(
                &SlackApiConversationsInfoRequest::new(SlackChannelId(channel_id.to_string())),
            )
            .await
        {
            Ok(channel_info) => {
                if let Some(name) = channel_info.channel.name {
                    channel_name_cache
                        .write()
                        .await
                        .insert(channel_id.to_string(), name.clone());
                    metadata.insert(
                        "slack_channel_name".into(),
                        serde_json::Value::String(name),
                    );
                }
            }
            // DM channels (D-prefixed) don't support conversations.info in all cases
            Err(error) if !channel_id.starts_with('D') => {
                tracing::warn!(
                    %error,
                    channel_id = %channel_id,
                    "failed to resolve Slack channel name; verify channels:read scope"
                );
            }
            Err(_) => {}
        }
    }

    // Resolve user identity via cache or users.info API.
    let mut formatted_author = user_id.map(|u| u.to_string());

    if let Some(uid) = slack_user_id {
        let cached = user_identity_cache.read().await.get(&uid.0).cloned();
        let identity = if let Some(identity) = cached {
            Some(identity)
        } else if let Ok(user_info) = session
            .users_info(&SlackApiUsersInfoRequest::new(uid.clone()))
            .await
        {
            let identity = resolve_slack_user_identity(&user_info.user, &uid.0);
            user_identity_cache
                .write()
                .await
                .insert(uid.0.clone(), identity.clone());
            Some(identity)
        } else {
            None
        };

        if let Some(identity) = identity {
            metadata.insert(
                "sender_display_name".into(),
                serde_json::Value::String(identity.display_name.clone()),
            );
            metadata.insert(
                "slack_user_mention".into(),
                serde_json::Value::String(format!("<@{}>", uid.0)),
            );
            if let Some(ref name) = identity.username {
                metadata.insert(
                    "sender_username".into(),
                    serde_json::Value::String(name.clone()),
                );
            }
            formatted_author = Some(identity.display_name.clone());
        }
    }

    // For DMs without a resolved channel name, use the sender's display name.
    if channel_id.starts_with('D') && !metadata.contains_key("slack_channel_name") {
        if let Some(display_name) = metadata
            .get("sender_display_name")
            .and_then(|v| v.as_str())
        {
            metadata.insert(
                "slack_channel_name".into(),
                serde_json::Value::String(format!("dm-{display_name}")),
            );
        }
    }

    (metadata, formatted_author)
}

/// Dispatch a fully-constructed `InboundMessage` to the inbound channel.
async fn send_inbound(
    tx: &mpsc::Sender<InboundMessage>,
    ts: String,
    conversation_id: String,
    sender_id: String,
    content: MessageContent,
    metadata: HashMap<String, serde_json::Value>,
    formatted_author: Option<String>,
) {
    let inbound = InboundMessage {
        id: ts,
        source: "slack".into(),
        conversation_id,
        sender_id,
        agent_id: None,
        content,
        timestamp: chrono::Utc::now(),
        metadata,
        formatted_author,
    };
    if let Err(error) = tx.send(inbound).await {
        tracing::warn!(%error, "failed to send inbound message from Slack");
    }
}

/// Deserialise a `Vec<serde_json::Value>` into `Vec<SlackBlock>`.
///
/// Blocks that fail to deserialise are silently skipped with a warning so a
/// single bad block doesn't kill the whole message.
fn deserialize_blocks(values: &[serde_json::Value]) -> Vec<SlackBlock> {
    values
        .iter()
        .filter_map(|v| match serde_json::from_value::<SlackBlock>(v.clone()) {
            Ok(block) => Some(block),
            Err(err) => {
                tracing::warn!(error = %err, "failed to deserialise slack block, skipping");
                None
            }
        })
        .collect()
}

/// Strip the leading `<@BOT_USER_ID>` mention from an `app_mention` event text.
///
/// Slack always formats user IDs in uppercase (e.g. `<@U012AB3CD>`), so a
/// simple prefix strip is sufficient — no case-folding is needed.
fn strip_bot_mention(text: &str, bot_user_id: &str) -> String {
    let mention = format!("<@{}>", bot_user_id);
    text.trim_start_matches(mention.as_str())
        .trim_start()
        .to_string()
}

/// Return a short human-readable name for an `OutboundResponse` variant for log messages.
fn variant_name(response: &OutboundResponse) -> &'static str {
    match response {
        OutboundResponse::Text(_) => "Text",
        OutboundResponse::ThreadReply { .. } => "ThreadReply",
        OutboundResponse::File { .. } => "File",
        OutboundResponse::Reaction(_) => "Reaction",
        OutboundResponse::RemoveReaction(_) => "RemoveReaction",
        OutboundResponse::Ephemeral { .. } => "Ephemeral",
        OutboundResponse::RichMessage { .. } => "RichMessage",
        OutboundResponse::ScheduledMessage { .. } => "ScheduledMessage",
        OutboundResponse::StreamStart => "StreamStart",
        OutboundResponse::StreamChunk(_) => "StreamChunk",
        OutboundResponse::StreamEnd => "StreamEnd",
        OutboundResponse::Status(_) => "Status",
    }
}

/// Split a message into UTF-8-safe chunks at line/word boundaries.
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

        // Walk back to a valid char boundary before slicing
        let mut safe_max = max_len.min(remaining.len());
        while !remaining.is_char_boundary(safe_max) {
            safe_max -= 1;
        }

        let split_at = remaining[..safe_max]
            .rfind('\n')
            .or_else(|| remaining[..safe_max].rfind(' '))
            .unwrap_or(safe_max);

        chunks.push(remaining[..split_at].to_string());
        remaining = remaining[split_at..].trim_start();
    }

    chunks
}

/// Sanitize an emoji name for Slack reactions (strip colons, lowercase).
fn sanitize_reaction_name(emoji: &str) -> String {
    emoji
        .trim()
        .trim_start_matches(':')
        .trim_end_matches(':')
        .to_lowercase()
}

fn resolve_slack_user_identity(user: &SlackUser, user_id: &str) -> SlackUserIdentity {
    let username = user.name.clone().filter(|n| !n.trim().is_empty());
    let display_name = user
        .profile
        .as_ref()
        .and_then(|p| p.display_name.clone().or_else(|| p.real_name.clone()))
        .filter(|n| !n.trim().is_empty())
        .or_else(|| username.clone())
        .unwrap_or_else(|| user_id.to_string());
    SlackUserIdentity {
        display_name,
        username,
    }
}
