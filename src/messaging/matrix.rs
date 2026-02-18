//! Matrix messaging adapter using matrix-sdk.

use crate::config::MatrixPermissions;
use crate::messaging::traits::{HistoryMessage, InboundStream, Messaging};
use crate::{Attachment, InboundMessage, MessageContent, OutboundResponse, StatusUpdate};

use anyhow::Context as _;
use arc_swap::ArcSwap;
use matrix_sdk::config::SyncSettings;
use matrix_sdk::matrix_auth::{MatrixSession, MatrixSessionTokens};
use matrix_sdk::room::edit::EditedContent;
use matrix_sdk::ruma::events::reaction::ReactionEventContent;
use matrix_sdk::ruma::events::relation::Annotation;
use matrix_sdk::ruma::events::room::member::StrippedRoomMemberEvent;
use matrix_sdk::ruma::events::room::message::{
    RoomMessageEventContent, RoomMessageEventContentWithoutRelation, SyncRoomMessageEvent,
};
use matrix_sdk::ruma::{OwnedEventId, OwnedRoomId, RoomId};
use matrix_sdk::{Client, SessionMeta};

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{RwLock, mpsc};
use tokio::task::JoinHandle;

/// Matrix adapter state.
pub struct MatrixAdapter {
    user_id: String,
    client: Client,
    permissions: Arc<ArcSwap<MatrixPermissions>>,
    auto_join: bool,
    /// Tracks in-progress streaming message edits per conversation_id.
    active_streams: Arc<RwLock<HashMap<String, ActiveStream>>>,
    /// Repeating typing indicator tasks per conversation_id.
    typing_tasks: Arc<RwLock<HashMap<String, JoinHandle<()>>>>,
    /// Shutdown signal for the sync loop.
    shutdown_tx: Arc<RwLock<Option<mpsc::Sender<()>>>>,
}

/// Tracks an in-progress streaming message edit.
struct ActiveStream {
    room_id: OwnedRoomId,
    event_id: OwnedEventId,
    accumulated_text: String,
    last_edit: Instant,
}

/// Matrix's recommended per-message character limit.
const MAX_MESSAGE_LENGTH: usize = 4096;

/// Minimum interval between streaming edits.
const STREAM_EDIT_INTERVAL: Duration = Duration::from_millis(300);

impl MatrixAdapter {
    /// Create a new Matrix adapter. Logs in or restores session as appropriate.
    pub async fn new(
        config: &crate::config::MatrixConfig,
        data_dir: PathBuf,
        permissions: Arc<ArcSwap<MatrixPermissions>>,
    ) -> anyhow::Result<Self> {
        std::fs::create_dir_all(&data_dir)
            .with_context(|| format!("failed to create matrix data dir: {}", data_dir.display()))?;

        let client = Client::builder()
            .homeserver_url(&config.homeserver_url)
            .sqlite_store(&data_dir, None)
            .build()
            .await
            .context("failed to build matrix client")?;

        let session_path = data_dir.join("session.json");

        if session_path.exists() {
            // Preserves device ID and E2EE keys across restarts
            let session_data = std::fs::read_to_string(&session_path)
                .context("failed to read matrix session.json")?;
            let session: MatrixSession = serde_json::from_str(&session_data)
                .context("failed to parse matrix session.json")?;
            client.restore_session(session).await
                .context("failed to restore matrix session")?;
            tracing::info!("matrix session restored from disk");
        } else if let Some(access_token) = &config.access_token {
            let user_id = matrix_sdk::ruma::UserId::parse(&config.user_id)
                .context("invalid matrix user_id")?;
            let device_id = matrix_sdk::ruma::device_id!("SPACEBOT").to_owned();
            let session = MatrixSession {
                meta: SessionMeta {
                    user_id: user_id.to_owned(),
                    device_id,
                },
                tokens: MatrixSessionTokens {
                    access_token: access_token.clone(),
                    refresh_token: None,
                },
            };
            client.restore_session(session).await
                .context("failed to restore matrix session from access token")?;
            tracing::info!("matrix session restored from access token");
        } else if let Some(password) = &config.password {
            client
                .matrix_auth()
                .login_username(&config.user_id, password)
                .initial_device_display_name("spacebot")
                .await
                .context("matrix password login failed")?;
            tracing::info!("matrix logged in with password");
        } else {
            anyhow::bail!("matrix: no session file, access_token, or password configured");
        }

        // Future restarts can skip login by restoring the session from disk
        if let Some(session) = client.session()
            && let matrix_sdk::AuthSession::Matrix(matrix_session) = session
        {
            let json = serde_json::to_string_pretty(&matrix_session)
                .context("failed to serialize matrix session")?;
            std::fs::write(&session_path, json)
                .context("failed to write matrix session.json")?;
        }

        Ok(Self {
            user_id: config.user_id.clone(),
            client,
            permissions,
            auto_join: config.auto_join,
            active_streams: Arc::new(RwLock::new(HashMap::new())),
            typing_tasks: Arc::new(RwLock::new(HashMap::new())),
            shutdown_tx: Arc::new(RwLock::new(None)),
        })
    }

    fn extract_room_id(&self, message: &InboundMessage) -> anyhow::Result<OwnedRoomId> {
        let id_str = message
            .metadata
            .get("matrix_room_id")
            .and_then(|v| v.as_str())
            .context("missing matrix_room_id in metadata")?;
        let room_id = <&RoomId>::try_from(id_str)
            .context("invalid matrix_room_id")?;
        Ok(room_id.to_owned())
    }

    fn extract_event_id(&self, message: &InboundMessage) -> anyhow::Result<OwnedEventId> {
        let id_str = message
            .metadata
            .get("matrix_event_id")
            .and_then(|v| v.as_str())
            .context("missing matrix_event_id in metadata")?;
        let event_id = OwnedEventId::try_from(id_str.to_string())
            .context("invalid matrix_event_id")?;
        Ok(event_id)
    }

    fn get_room(&self, room_id: &RoomId) -> anyhow::Result<matrix_sdk::Room> {
        self.client
            .get_room(room_id)
            .context("room not found (not joined?)")
    }

    async fn stop_typing(&self, conversation_id: &str) {
        if let Some(handle) = self.typing_tasks.write().await.remove(conversation_id) {
            handle.abort();
        }
    }
}

impl Messaging for MatrixAdapter {
    fn name(&self) -> &str {
        "matrix"
    }

    async fn start(&self) -> crate::Result<InboundStream> {
        let (inbound_tx, inbound_rx) = mpsc::channel(256);
        let (shutdown_tx, mut shutdown_rx) = mpsc::channel::<()>(1);

        *self.shutdown_tx.write().await = Some(shutdown_tx);

        let our_user_id = self.user_id.clone();
        let permissions = self.permissions.clone();

        // Register message event handler
        let tx = inbound_tx.clone();
        let our_user_id_clone = our_user_id.clone();
        let permissions_clone = permissions.clone();
        let homeserver = self
            .client
            .homeserver()
            .to_string();

        self.client.add_event_handler(
            move |event: SyncRoomMessageEvent, room: matrix_sdk::Room| {
                let tx = tx.clone();
                let our_user_id = our_user_id_clone.clone();
                let permissions = permissions_clone.clone();
                let homeserver = homeserver.clone();
                async move {
                    let original = match event {
                        SyncRoomMessageEvent::Original(o) => o,
                        SyncRoomMessageEvent::Redacted(_) => return,
                    };

                    // Skip own messages
                    if original.sender.as_str() == our_user_id {
                        return;
                    }

                    let room_id = room.room_id().to_string();
                    let permissions = permissions.load();

                    // Room filter
                    if let Some(ref filter) = permissions.room_filter
                        && !filter.iter().any(|id| id == &room_id)
                    {
                        return;
                    }

                    // DM detection and filter
                    let is_dm = room.is_direct().await.unwrap_or(false);
                    if is_dm
                        && !permissions.dm_allowed_users.is_empty()
                        && !permissions.dm_allowed_users.iter().any(|u| u == original.sender.as_str())
                    {
                        return;
                    }

                    let conversation_id = if is_dm {
                        format!("matrix:dm:{}", original.sender)
                    } else {
                        format!("matrix:{room_id}")
                    };

                    // Extract text content and attachments
                    let (text, attachments) = extract_content(&original.content, &homeserver);

                    if text.is_none() && attachments.is_empty() {
                        return;
                    }

                    let content = if attachments.is_empty() {
                        MessageContent::Text(text.unwrap_or_default())
                    } else {
                        MessageContent::Media {
                            text,
                            attachments,
                        }
                    };

                    let mut metadata = HashMap::new();
                    metadata.insert("matrix_room_id".into(), serde_json::Value::String(room_id));
                    metadata.insert(
                        "matrix_event_id".into(),
                        serde_json::Value::String(original.event_id.to_string()),
                    );
                    metadata.insert(
                        "matrix_sender".into(),
                        serde_json::Value::String(original.sender.to_string()),
                    );
                    metadata.insert(
                        "display_name".into(),
                        serde_json::Value::String(original.sender.localpart().to_string()),
                    );
                    if is_dm {
                        metadata.insert(
                            "matrix_is_dm".into(),
                            serde_json::Value::Bool(true),
                        );
                    }
                    if let Some(name) = room.cached_display_name() {
                        metadata.insert(
                            "matrix_room_name".into(),
                            serde_json::Value::String(name.to_string()),
                        );
                    }

                    let timestamp = chrono::DateTime::from_timestamp_millis(
                        original.origin_server_ts.0.into(),
                    )
                    .unwrap_or_else(chrono::Utc::now);

                    let inbound = InboundMessage {
                        id: original.event_id.to_string(),
                        source: "matrix".into(),
                        conversation_id,
                        sender_id: original.sender.to_string(),
                        agent_id: None,
                        content,
                        timestamp,
                        metadata,
                    };

                    if let Err(error) = tx.send(inbound).await {
                        tracing::warn!(
                            %error,
                            "failed to send inbound message from Matrix (receiver dropped)"
                        );
                    }
                }
            },
        );

        if self.auto_join {
            self.client.add_event_handler(
                |event: StrippedRoomMemberEvent, client: Client, room: matrix_sdk::Room| async move {
                    // Only act on invites for ourselves
                    if let Some(user_id) = client.user_id()
                        && event.state_key != *user_id
                    {
                        return;
                    }
                    if room.state() == matrix_sdk::RoomState::Invited {
                        tracing::info!(room_id = %room.room_id(), "auto-joining invited room");
                        // Retry join a few times in case of federation lag
                        for attempt in 0..3 {
                            match room.join().await {
                                Ok(_) => {
                                    tracing::info!(room_id = %room.room_id(), "joined room");
                                    return;
                                }
                                Err(error) => {
                                    tracing::warn!(
                                        %error,
                                        attempt,
                                        room_id = %room.room_id(),
                                        "failed to join room, retrying"
                                    );
                                    tokio::time::sleep(Duration::from_secs(1)).await;
                                }
                            }
                        }
                    }
                },
            );
        }

        let client = self.client.clone();
        tokio::spawn(async move {
            let settings = SyncSettings::default();
            tokio::select! {
                _ = shutdown_rx.recv() => {
                    tracing::info!("matrix sync loop shutting down");
                }
                _ = client.sync(settings) => {
                    tracing::warn!("matrix sync loop ended unexpectedly");
                }
            }
        });

        tracing::info!(user_id = %self.user_id, "matrix connected");
        let stream = tokio_stream::wrappers::ReceiverStream::new(inbound_rx);
        Ok(Box::pin(stream))
    }

    async fn respond(
        &self,
        message: &InboundMessage,
        response: OutboundResponse,
    ) -> crate::Result<()> {
        match response {
            OutboundResponse::Text(text) => {
                self.stop_typing(&message.conversation_id).await;
                let room_id = self.extract_room_id(message)?;
                let room = self.get_room(&room_id)?;

                for chunk in split_message(&text, MAX_MESSAGE_LENGTH) {
                    let content = RoomMessageEventContent::text_plain(&chunk);
                    room.send(content).await
                        .context("failed to send matrix message")?;
                }
            }
            OutboundResponse::ThreadReply { thread_name: _, text } => {
                self.stop_typing(&message.conversation_id).await;
                let room_id = self.extract_room_id(message)?;
                let room = self.get_room(&room_id)?;

                for chunk in split_message(&text, MAX_MESSAGE_LENGTH) {
                    let content = RoomMessageEventContent::text_plain(&chunk);
                    // Matrix doesn't have named threads like Slack. Send as reply.
                    room.send(content).await
                        .context("failed to send matrix thread reply")?;
                }
            }
            OutboundResponse::File {
                filename,
                data,
                mime_type,
                caption,
            } => {
                self.stop_typing(&message.conversation_id).await;
                let room_id = self.extract_room_id(message)?;
                let room = self.get_room(&room_id)?;

                let content_type: mime_guess::mime::Mime = mime_type
                    .parse()
                    .unwrap_or(mime_guess::mime::APPLICATION_OCTET_STREAM);

                let config = matrix_sdk::attachment::AttachmentConfig::new();
                room.send_attachment(&filename, &content_type, data, config)
                    .await
                    .context("failed to send matrix file attachment")?;

                // Send caption as a follow-up message if provided
                if let Some(caption_text) = caption
                    && !caption_text.is_empty()
                {
                    let caption_content = RoomMessageEventContent::text_plain(&caption_text);
                    room.send(caption_content).await
                        .context("failed to send matrix file caption")?;
                }
            }
            OutboundResponse::Reaction(emoji) => {
                let room_id = self.extract_room_id(message)?;
                let room = self.get_room(&room_id)?;
                let event_id = self.extract_event_id(message)?;

                let annotation = Annotation::new(event_id, emoji.clone());
                let content = ReactionEventContent::new(annotation);
                if let Err(error) = room.send(content).await {
                    tracing::debug!(
                        %error,
                        emoji = %emoji,
                        "failed to send matrix reaction"
                    );
                }
            }
            OutboundResponse::StreamStart => {
                self.stop_typing(&message.conversation_id).await;
                let room_id = self.extract_room_id(message)?;
                let room = self.get_room(&room_id)?;

                let placeholder = RoomMessageEventContent::text_plain("...");
                let response = room.send(placeholder).await
                    .context("failed to send matrix stream placeholder")?;

                self.active_streams.write().await.insert(
                    message.conversation_id.clone(),
                    ActiveStream {
                        room_id,
                        event_id: response.event_id,
                        accumulated_text: String::new(),
                        last_edit: Instant::now(),
                    },
                );
            }
            OutboundResponse::StreamChunk(text) => {
                let mut active = self.active_streams.write().await;
                if let Some(stream) = active.get_mut(&message.conversation_id) {
                    stream.accumulated_text = text;

                    // Rate-limit edits
                    if stream.last_edit.elapsed() < STREAM_EDIT_INTERVAL {
                        return Ok(());
                    }

                    let display_text = if stream.accumulated_text.len() > MAX_MESSAGE_LENGTH {
                        let end = stream.accumulated_text
                            .floor_char_boundary(MAX_MESSAGE_LENGTH - 3);
                        format!("{}...", &stream.accumulated_text[..end])
                    } else {
                        stream.accumulated_text.clone()
                    };

                    let room = self.get_room(&stream.room_id)?;
                    let edited = EditedContent::RoomMessage(
                        RoomMessageEventContentWithoutRelation::text_plain(&display_text),
                    );
                    match room.make_edit_event(&stream.event_id, edited).await {
                        Ok(edit_event) => {
                            if let Err(error) = room.send(edit_event).await {
                                tracing::debug!(%error, "failed to edit streaming matrix message");
                            }
                        }
                        Err(error) => {
                            tracing::debug!(%error, "failed to create matrix edit event");
                        }
                    }
                    stream.last_edit = Instant::now();
                }
            }
            OutboundResponse::StreamEnd => {
                let stream = self
                    .active_streams
                    .write()
                    .await
                    .remove(&message.conversation_id);
                if let Some(stream) = stream
                    && !stream.accumulated_text.is_empty()
                {
                    let room = self.get_room(&stream.room_id)?;
                    for (i, chunk) in split_message(&stream.accumulated_text, MAX_MESSAGE_LENGTH)
                        .iter()
                        .enumerate()
                    {
                        if i == 0 {
                            let edited = EditedContent::RoomMessage(
                                RoomMessageEventContentWithoutRelation::text_plain(chunk),
                            );
                            match room.make_edit_event(&stream.event_id, edited).await {
                                Ok(edit_event) => {
                                    if let Err(error) = room.send(edit_event).await {
                                        tracing::debug!(%error, "failed to send final matrix stream edit");
                                    }
                                }
                                Err(error) => {
                                    tracing::debug!(%error, "failed to create final matrix edit event");
                                }
                            }
                        } else {
                            let content = RoomMessageEventContent::text_plain(chunk);
                            if let Err(error) = room.send(content).await {
                                tracing::debug!(%error, "failed to send matrix overflow chunk");
                            }
                        }
                    }
                }
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
                let room_id = self.extract_room_id(message)?;
                let room = self.get_room(&room_id)?;
                let conversation_id = message.conversation_id.clone();

                // Matrix typing indicators expire after ~4 seconds.
                // Send one immediately, then repeat every 4 seconds.
                let handle = tokio::spawn(async move {
                    loop {
                        if let Err(error) = room.typing_notice(true).await {
                            tracing::debug!(%error, "failed to send matrix typing indicator");
                            break;
                        }
                        tokio::time::sleep(Duration::from_secs(4)).await;
                    }
                });

                self.typing_tasks
                    .write()
                    .await
                    .insert(conversation_id, handle);
            }
            StatusUpdate::StopTyping
            | StatusUpdate::ToolStarted { .. }
            | StatusUpdate::ToolCompleted { .. }
            | StatusUpdate::BranchStarted { .. }
            | StatusUpdate::WorkerStarted { .. }
            | StatusUpdate::WorkerCompleted { .. } => {
                self.stop_typing(&message.conversation_id).await;
            }
        }

        Ok(())
    }

    async fn broadcast(
        &self,
        target: &str,
        response: OutboundResponse,
    ) -> crate::Result<()> {
        let room_id = <&RoomId>::try_from(target)
            .context("invalid matrix room ID for broadcast")?;
        let room = self.get_room(room_id)?;

        match response {
            OutboundResponse::Text(text) => {
                for chunk in split_message(&text, MAX_MESSAGE_LENGTH) {
                    let content = RoomMessageEventContent::text_plain(&chunk);
                    room.send(content).await
                        .context("failed to broadcast matrix message")?;
                }
            }
            OutboundResponse::ThreadReply { .. }
            | OutboundResponse::File { .. }
            | OutboundResponse::Reaction(_)
            | OutboundResponse::StreamStart
            | OutboundResponse::StreamChunk(_)
            | OutboundResponse::StreamEnd
            | OutboundResponse::Status(_) => {
                tracing::debug!("unsupported broadcast response type for matrix");
            }
        }

        Ok(())
    }

    async fn fetch_history(
        &self,
        message: &InboundMessage,
        limit: usize,
    ) -> crate::Result<Vec<HistoryMessage>> {
        let room_id = self.extract_room_id(message)?;
        let room = self.get_room(&room_id)?;

        let our_user_id = self.user_id.clone();

        let options = matrix_sdk::room::MessagesOptions::backward()
            .from(None::<&str>);
        let messages = room.messages(options).await
            .context("failed to fetch matrix message history")?;

        let mut history = Vec::new();
        for event in messages.chunk.into_iter().rev() {
            let event = match event.raw().deserialize() {
                Ok(ev) => ev,
                Err(error) => {
                    tracing::debug!(%error, "failed to deserialize matrix timeline event");
                    continue;
                }
            };

            if let matrix_sdk::ruma::events::AnySyncTimelineEvent::MessageLike(
                matrix_sdk::ruma::events::AnySyncMessageLikeEvent::RoomMessage(msg_event),
            ) = event
            {
                let sender = msg_event.sender().to_string();
                let is_bot = sender == our_user_id;
                if let Some(original) = msg_event.as_original() {
                    let body = original.content.body().to_string();
                    history.push(HistoryMessage {
                        author: sender,
                        content: body,
                        is_bot,
                    });
                }
            }

            if history.len() >= limit {
                break;
            }
        }

        Ok(history)
    }

    async fn health_check(&self) -> crate::Result<()> {
        self.client
            .whoami()
            .await
            .context("matrix health check failed")?;
        Ok(())
    }

    async fn shutdown(&self) -> crate::Result<()> {
        let mut tasks = self.typing_tasks.write().await;
        for (_, handle) in tasks.drain() {
            handle.abort();
        }

        if let Some(tx) = self.shutdown_tx.read().await.as_ref() {
            tx.send(()).await.ok();
        }

        tracing::info!("matrix adapter shut down");
        Ok(())
    }
}

/// Extract text content and attachments from a Matrix message.
fn extract_content(
    content: &RoomMessageEventContent,
    homeserver: &str,
) -> (Option<String>, Vec<Attachment>) {
    use matrix_sdk::ruma::events::room::message::MessageType;

    let mut attachments = Vec::new();

    match &content.msgtype {
        MessageType::Text(text) => {
            return (Some(text.body.clone()), attachments);
        }
        MessageType::Notice(notice) => {
            return (Some(notice.body.clone()), attachments);
        }
        MessageType::Emote(emote) => {
            return (Some(format!("* {}", emote.body)), attachments);
        }
        MessageType::Image(image) => {
            let url = media_source_url(&image.source, homeserver);
            attachments.push(Attachment {
                filename: image.body.clone(),
                mime_type: image
                    .info
                    .as_ref()
                    .and_then(|i| i.mimetype.clone())
                    .unwrap_or_else(|| "image/png".to_string()),
                url,
                size_bytes: image.info.as_ref().and_then(|i| i.size).map(|s| s.into()),
            });
        }
        MessageType::File(file) => {
            let url = media_source_url(&file.source, homeserver);
            attachments.push(Attachment {
                filename: file.body.clone(),
                mime_type: file
                    .info
                    .as_ref()
                    .and_then(|i| i.mimetype.clone())
                    .unwrap_or_else(|| "application/octet-stream".to_string()),
                url,
                size_bytes: file.info.as_ref().and_then(|i| i.size).map(|s| s.into()),
            });
        }
        MessageType::Audio(audio) => {
            let url = media_source_url(&audio.source, homeserver);
            attachments.push(Attachment {
                filename: audio.body.clone(),
                mime_type: audio
                    .info
                    .as_ref()
                    .and_then(|i| i.mimetype.clone())
                    .unwrap_or_else(|| "audio/mpeg".to_string()),
                url,
                size_bytes: audio.info.as_ref().and_then(|i| i.size).map(|s| s.into()),
            });
        }
        MessageType::Video(video) => {
            let url = media_source_url(&video.source, homeserver);
            attachments.push(Attachment {
                filename: video.body.clone(),
                mime_type: video
                    .info
                    .as_ref()
                    .and_then(|i| i.mimetype.clone())
                    .unwrap_or_else(|| "video/mp4".to_string()),
                url,
                size_bytes: video.info.as_ref().and_then(|i| i.size).map(|s| s.into()),
            });
        }
        _ => {
            return (None, attachments);
        }
    }

    (None, attachments)
}

/// Extract a download URL from a MediaSource.
fn media_source_url(source: &matrix_sdk::ruma::events::room::MediaSource, homeserver: &str) -> String {
    match source {
        matrix_sdk::ruma::events::room::MediaSource::Plain(mxc_uri) => {
            resolve_mxc_url(homeserver, mxc_uri.as_str())
        }
        matrix_sdk::ruma::events::room::MediaSource::Encrypted(info) => {
            resolve_mxc_url(homeserver, info.url.as_str())
        }
    }
}

/// Convert an `mxc://` URL to an HTTP download URL.
fn resolve_mxc_url(homeserver: &str, mxc_url: &str) -> String {
    if let Some(stripped) = mxc_url.strip_prefix("mxc://") {
        let hs = homeserver.trim_end_matches('/');
        format!("{hs}/_matrix/media/v3/download/{stripped}")
    } else {
        mxc_url.to_string()
    }
}

/// Split a message into chunks that fit within Matrix's character limit.
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
