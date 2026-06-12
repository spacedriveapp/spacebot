//! Mattermost messaging adapter using a custom HTTP + WebSocket client.

use crate::config::MattermostPermissions;
use crate::messaging::apply_runtime_adapter_to_conversation_id;
use crate::messaging::traits::{HistoryMessage, InboundStream, Messaging};
use crate::{InboundMessage, MessageContent, OutboundResponse, StatusUpdate};

use anyhow::Context as _;
use arc_swap::ArcSwap;
use futures::{SinkExt, StreamExt};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{OnceCell, RwLock, mpsc};
use tokio_tungstenite::{connect_async, tungstenite::Message as WsMessage};
use url::Url;

const MAX_MESSAGE_LENGTH: usize = 16_383;
const STREAM_EDIT_THROTTLE: Duration = Duration::from_millis(500);
const TYPING_INDICATOR_INTERVAL: Duration = Duration::from_secs(5);
const WS_RECONNECT_BASE_DELAY: Duration = Duration::from_secs(1);
const WS_RECONNECT_MAX_DELAY: Duration = Duration::from_secs(60);
const HTTP_TIMEOUT: Duration = Duration::from_secs(30);

pub struct MattermostAdapter {
    runtime_key: Arc<str>,
    base_url: Url,
    token: Arc<str>,
    default_team_id: Option<Arc<str>>,
    max_attachment_bytes: usize,
    client: Client,
    permissions: Arc<ArcSwap<MattermostPermissions>>,
    bot_user_id: OnceCell<Arc<str>>,
    bot_username: OnceCell<Arc<str>>,
    user_identity_cache: Arc<RwLock<HashMap<String, String>>>,
    channel_name_cache: Arc<RwLock<HashMap<String, String>>>,
    dm_channel_cache: Arc<RwLock<HashMap<String, String>>>,
    active_messages: Arc<RwLock<HashMap<String, ActiveStream>>>,
    typing_tasks: Arc<RwLock<HashMap<String, tokio::task::JoinHandle<()>>>>,
    shutdown_tx: Arc<RwLock<Option<mpsc::Sender<()>>>>,
    ws_task: Arc<RwLock<Option<tokio::task::JoinHandle<()>>>>,
}

struct ActiveStream {
    post_id: Arc<str>,
    #[allow(dead_code)]
    channel_id: Arc<str>,
    last_edit: Instant,
    accumulated_text: String,
}

struct MessageBuildContext<'a> {
    runtime_key: &'a str,
    bot_user_id: &'a str,
    bot_username: &'a str,
    team_id: &'a Option<String>,
    permissions: &'a MattermostPermissions,
    display_name: Option<&'a str>,
    channel_name: Option<&'a str>,
}

impl std::fmt::Debug for MattermostAdapter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MattermostAdapter")
            .field("runtime_key", &self.runtime_key)
            .field("base_url", &self.base_url)
            .field("token", &"[REDACTED]")
            .field("default_team_id", &self.default_team_id)
            .field("max_attachment_bytes", &self.max_attachment_bytes)
            .finish()
    }
}

impl MattermostAdapter {
    /// Create a new [`MattermostAdapter`].
    ///
    /// `base_url` must be an origin URL with no path, query, or fragment
    /// (e.g. `https://mm.example.com`). Returns an error if the URL is
    /// malformed or includes a path component.
    ///
    /// `runtime_key` is the adapter's unique identifier within the messaging
    /// manager (e.g. `"mattermost"` or `"mattermost:myinstance"`).
    ///
    /// `default_team_id` is used as a fallback when a WS event does not carry
    /// a team ID in its broadcast envelope.
    pub fn new(
        runtime_key: impl Into<Arc<str>>,
        base_url: &str,
        token: impl Into<Arc<str>>,
        default_team_id: Option<Arc<str>>,
        max_attachment_bytes: usize,
        permissions: Arc<ArcSwap<MattermostPermissions>>,
    ) -> anyhow::Result<Self> {
        let base_url = Url::parse(base_url).context("invalid mattermost base_url")?;
        if base_url.path() != "/" || base_url.query().is_some() || base_url.fragment().is_some() {
            return Err(anyhow::anyhow!(
                "mattermost base_url must be an origin URL without path/query/fragment (got: {})",
                base_url
            ));
        }

        let client = Client::builder()
            .timeout(HTTP_TIMEOUT)
            .pool_idle_timeout(Duration::from_secs(30))
            .build()
            .context("failed to build HTTP client")?;

        Ok(Self {
            runtime_key: runtime_key.into(),
            base_url,
            token: token.into(),
            default_team_id,
            max_attachment_bytes,
            client,
            permissions,
            bot_user_id: OnceCell::new(),
            bot_username: OnceCell::new(),
            user_identity_cache: Arc::new(RwLock::new(HashMap::new())),
            channel_name_cache: Arc::new(RwLock::new(HashMap::new())),
            dm_channel_cache: Arc::new(RwLock::new(HashMap::new())),
            active_messages: Arc::new(RwLock::new(HashMap::new())),
            typing_tasks: Arc::new(RwLock::new(HashMap::new())),
            shutdown_tx: Arc::new(RwLock::new(None)),
            ws_task: Arc::new(RwLock::new(None)),
        })
    }

    fn api_url(&self, path: &str) -> Url {
        let mut url = self.base_url.clone();
        url.path_segments_mut()
            .expect("base_url is a valid base URL")
            .extend(["api", "v4"])
            .extend(path.trim_start_matches('/').split('/'));
        url
    }

    fn ws_url(&self) -> Url {
        let mut url = self.base_url.clone();
        url.set_scheme(match self.base_url.scheme() {
            "https" => "wss",
            "http" => "ws",
            other => unreachable!("unsupported URL scheme: {other}"),
        })
        .expect("scheme substitution is valid");
        url.path_segments_mut()
            .expect("base_url is a valid base URL")
            .extend(["api", "v4", "websocket"]);
        url
    }

    fn extract_channel_id<'a>(&self, message: &'a InboundMessage) -> crate::Result<&'a str> {
        message
            .metadata
            .get("mattermost_channel_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("missing mattermost_channel_id metadata").into())
    }

    /// Validate a Mattermost resource ID (post, channel, user, etc.).
    ///
    /// A valid ID is 1–64 ASCII alphanumeric characters, hyphens, or
    /// underscores. Returns an error for empty strings, IDs that are too long,
    /// or IDs that contain characters outside that set (e.g. colons in
    /// `dm:{user_id}` composite targets).
    fn validate_id(id: &str) -> crate::Result<()> {
        if id.is_empty() || id.len() > 64 {
            return Err(anyhow::anyhow!("invalid mattermost ID: empty or too long").into());
        }
        if !id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            return Err(anyhow::anyhow!("invalid mattermost ID format: {id}").into());
        }
        Ok(())
    }

    /// Cancel any active typing indicator for `channel_id`.
    ///
    /// Aborts the background loop that posts `/users/{id}/typing` and removes
    /// it from the task map. No-op if no indicator is running.
    async fn stop_typing(&self, channel_id: &str) {
        if let Some(handle) = self.typing_tasks.write().await.remove(channel_id) {
            handle.abort();
        }
    }

    /// Start a repeating typing indicator for `channel_id`.
    ///
    /// Spawns a background task that posts `/users/{bot_id}/typing` every
    /// [`TYPING_INDICATOR_INTERVAL`]. Any previously running indicator for the
    /// same channel is aborted first to prevent task leaks. The task runs until
    /// [`stop_typing`](Self::stop_typing) is called or the adapter shuts down.
    ///
    /// Does nothing if `bot_user_id` has not been set (i.e. [`start`](Self::start)
    /// has not been called yet).
    async fn start_typing(&self, channel_id: &str) {
        self.stop_typing(channel_id).await;
        let Some(user_id) = self.bot_user_id.get().cloned() else {
            return;
        };
        let channel_id_owned = channel_id.to_string();
        let client = self.client.clone();
        let token = self.token.clone();
        let url = self.api_url(&format!("/users/{user_id}/typing"));

        let handle = tokio::spawn(async move {
            let mut interval = tokio::time::interval(TYPING_INDICATOR_INTERVAL);
            loop {
                interval.tick().await;
                let result = client
                    .post(url.clone())
                    .bearer_auth(token.as_ref())
                    .json(&serde_json::json!({
                        "channel_id": channel_id_owned,
                        "parent_id": "",
                    }))
                    .send()
                    .await;
                if let Err(error) = result {
                    tracing::warn!(%error, "typing indicator request failed");
                }
            }
        });

        self.typing_tasks
            .write()
            .await
            .insert(channel_id.to_string(), handle);
    }

    /// Create a new post in `channel_id` and return the created post.
    ///
    /// Pass `root_id` to place the post inside an existing thread; the root ID
    /// must be the ID of the first post in that thread (not a reply). Pass
    /// `None` to create a top-level post.
    ///
    /// Both `channel_id` and `root_id` (if provided) are validated with
    /// [`validate_id`](Self::validate_id) before the request is sent.
    async fn create_post(
        &self,
        channel_id: &str,
        message: &str,
        root_id: Option<&str>,
    ) -> crate::Result<MattermostPost> {
        Self::validate_id(channel_id)?;
        if let Some(rid) = root_id {
            Self::validate_id(rid)?;
        }

        let response = self
            .client
            .post(self.api_url("/posts"))
            .bearer_auth(self.token.as_ref())
            .json(&serde_json::json!({
                "channel_id": channel_id,
                "message": message,
                "root_id": root_id.unwrap_or(""),
            }))
            .send()
            .await
            .context("failed to create post")?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(anyhow::anyhow!(
                "mattermost POST /posts failed with status {}: {body}",
                status.as_u16()
            )
            .into());
        }

        response
            .json()
            .await
            .context("failed to parse post response")
            .map_err(Into::into)
    }

    /// Replace the text of an existing post in-place.
    ///
    /// Used by the streaming path to update the placeholder post created by
    /// [`StreamStart`](crate::OutboundResponse::StreamStart) as chunks arrive,
    /// and to finalize it on [`StreamEnd`](crate::OutboundResponse::StreamEnd).
    async fn edit_post(&self, post_id: &str, message: &str) -> crate::Result<()> {
        Self::validate_id(post_id)?;

        let response = self
            .client
            .put(self.api_url(&format!("/posts/{post_id}")))
            .bearer_auth(self.token.as_ref())
            .json(&serde_json::json!({ "message": message }))
            .send()
            .await
            .context("failed to edit post")?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(anyhow::anyhow!(
                "mattermost PUT /posts/{post_id} failed with status {}: {body}",
                status.as_u16()
            )
            .into());
        }

        Ok(())
    }

    /// Fetch up to `limit` posts from `channel_id`, sorted by creation time.
    ///
    /// Pass `before_post_id` to retrieve posts that appeared before a specific
    /// post (exclusive), which is used by [`fetch_history`](Self::fetch_history)
    /// to anchor the history window to the triggering message. The Mattermost
    /// API always returns the most recent matching posts first; callers are
    /// responsible for reversing the order if needed.
    async fn get_channel_posts(
        &self,
        channel_id: &str,
        before_post_id: Option<&str>,
        limit: u32,
    ) -> crate::Result<MattermostPostList> {
        Self::validate_id(channel_id)?;

        let mut url = self.api_url(&format!("/channels/{channel_id}/posts"));
        {
            let mut query = url.query_pairs_mut();
            query.append_pair("page", "0");
            query.append_pair("per_page", &limit.to_string());
            if let Some(before) = before_post_id {
                query.append_pair("before", before);
            }
        }

        let response = self
            .client
            .get(url)
            .bearer_auth(self.token.as_ref())
            .send()
            .await
            .context("failed to fetch channel posts")?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(anyhow::anyhow!(
                "mattermost GET /channels/{channel_id}/posts failed with status {}: {body}",
                status.as_u16()
            )
            .into());
        }

        response
            .json()
            .await
            .context("failed to parse posts response")
            .map_err(Into::into)
    }

    /// Return the Mattermost channel ID for a direct message conversation with
    /// `user_id`, creating it via the API if it does not exist yet.
    ///
    /// Results are cached in `dm_channel_cache` so that subsequent calls for
    /// the same user avoid a round-trip. Requires [`start`](Self::start) to
    /// have been called so that `bot_user_id` is available.
    async fn get_or_create_dm_channel(&self, user_id: &str) -> crate::Result<String> {
        if let Some(channel_id) = self.dm_channel_cache.read().await.get(user_id).cloned() {
            return Ok(channel_id);
        }
        let bot_user_id = self
            .bot_user_id
            .get()
            .ok_or_else(|| anyhow::anyhow!("bot_user_id not initialized"))?
            .as_ref()
            .to_string();
        let response = self
            .client
            .post(self.api_url("/channels/direct"))
            .bearer_auth(self.token.as_ref())
            .json(&serde_json::json!([bot_user_id, user_id]))
            .send()
            .await
            .context("failed to create DM channel")?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(anyhow::anyhow!(
                "mattermost POST /channels/direct failed with status {}: {body}",
                status.as_u16()
            )
            .into());
        }
        let channel: MattermostChannel = response
            .json()
            .await
            .context("failed to parse DM channel response")?;
        self.dm_channel_cache
            .write()
            .await
            .insert(user_id.to_string(), channel.id.clone());
        Ok(channel.id)
    }
}

impl Messaging for MattermostAdapter {
    fn name(&self) -> &str {
        &self.runtime_key
    }

    async fn start(&self) -> crate::Result<InboundStream> {
        let (inbound_tx, inbound_rx) = mpsc::channel(256);
        let (shutdown_tx, mut shutdown_rx) = mpsc::channel::<()>(1);
        *self.shutdown_tx.write().await = Some(shutdown_tx);

        let me_response = self
            .client
            .get(self.api_url("/users/me"))
            .bearer_auth(self.token.as_ref())
            .send()
            .await
            .context("failed to get bot user")?;
        let me_status = me_response.status();
        if !me_status.is_success() {
            let body = me_response.text().await.unwrap_or_default();
            return Err(anyhow::anyhow!(
                "mattermost /users/me failed with status {}: {body}",
                me_status.as_u16()
            )
            .into());
        }
        let me: MattermostUser = me_response
            .json()
            .await
            .context("failed to parse user response")?;

        let user_id: Arc<str> = me.id.clone().into();
        let username: Arc<str> = me.username.clone().into();

        if self.bot_user_id.set(user_id.clone()).is_err() {
            tracing::warn!(adapter = %self.runtime_key, "bot_user_id already initialized — start() called more than once");
        }
        if self.bot_username.set(username.clone()).is_err() {
            tracing::warn!(adapter = %self.runtime_key, "bot_username already initialized — start() called more than once");
        }

        tracing::info!(
            adapter = %self.runtime_key,
            bot_id = %user_id,
            bot_username = %username,
            "mattermost adapter connected"
        );

        let ws_url = self.ws_url();
        let runtime_key = self.runtime_key.clone();
        let token = self.token.clone();
        let permissions = self.permissions.clone();
        let bot_user_id = user_id;
        let bot_username_ws = username;
        let user_identity_cache = self.user_identity_cache.clone();
        let channel_name_cache = self.channel_name_cache.clone();
        let ws_client = self.client.clone();
        let ws_base_url = self.base_url.clone();
        let inbound_tx_clone = inbound_tx.clone();
        let default_team_id = self.default_team_id.clone();

        let handle = tokio::spawn(async move {
            let mut retry_delay = WS_RECONNECT_BASE_DELAY;

            loop {
                let connect_result = connect_async(ws_url.as_str()).await;

                match connect_result {
                    Ok((ws_stream, _)) => {
                        retry_delay = WS_RECONNECT_BASE_DELAY;

                        let (mut write, mut read) = ws_stream.split();

                        let auth_msg = serde_json::json!({
                            "seq": 1,
                            "action": "authentication_challenge",
                            "data": {"token": token.as_ref()}
                        });

                        if let Ok(msg) = serde_json::to_string(&auth_msg)
                            && write.send(WsMessage::Text(msg.into())).await.is_err()
                        {
                            tracing::error!(adapter = %runtime_key, "failed to send websocket auth");
                            continue;
                        }

                        loop {
                            tokio::select! {
                                _ = shutdown_rx.recv() => {
                                    tracing::info!(adapter = %runtime_key, "mattermost websocket shutting down");
                                    if let Err(error) = write.send(WsMessage::Close(None)).await {
                                        tracing::debug!(%error, "failed to send websocket close frame");
                                    }
                                    return;
                                }

                                msg = read.next() => {
                                    match msg {
                                        Some(Ok(WsMessage::Text(text))) => {
                                            match serde_json::from_str::<MattermostWsEvent>(&text) {
                                            Err(error) => {
                                                tracing::debug!(%error, text = text.as_str(), "failed to parse Mattermost WS event envelope");
                                            }
                                            Ok(event) => {
                                            if event.event == "posted" {
                                                    // The post is double-encoded as a JSON string in the data field.
                                                    let post_result = event
                                                        .data
                                                        .get("post")
                                                        .and_then(|v| v.as_str())
                                                        .map(serde_json::from_str::<MattermostPost>);

                                                    let post_result = match post_result {
                                                        Some(Ok(p)) => Some(p),
                                                        Some(Err(error)) => {
                                                            tracing::debug!(%error, "failed to parse Mattermost WS post payload");
                                                            None
                                                        }
                                                        None => None,
                                                    };

                                                    if let Some(mut post) = post_result
                                                        && post.user_id != bot_user_id.as_ref()
                                                    {
                                                        // channel_type comes from event.data, not the post struct.
                                                        let channel_type = event
                                                            .data
                                                            .get("channel_type")
                                                            .and_then(|v| v.as_str())
                                                            .map(String::from);
                                                        post.channel_type = channel_type;

                                                        let team_id = event.broadcast.team_id.clone()
                                                            .or_else(|| default_team_id.as_ref().map(|s| s.to_string()));
                                                        let perms = permissions.load();

                                                        let display_name = resolve_user_display_name(
                                                            &user_identity_cache,
                                                            &ws_client,
                                                            token.as_ref(),
                                                            &ws_base_url,
                                                            &post.user_id,
                                                        ).await;
                                                        let channel_name = resolve_channel_name(
                                                            &channel_name_cache,
                                                            &ws_client,
                                                            token.as_ref(),
                                                            &ws_base_url,
                                                            &post.channel_id,
                                                        ).await;

                                                        let message_context = MessageBuildContext {
                                                            runtime_key: &runtime_key,
                                                            bot_user_id: &bot_user_id,
                                                            bot_username: &bot_username_ws,
                                                            team_id: &team_id,
                                                            permissions: &perms,
                                                            display_name: display_name.as_deref(),
                                                            channel_name: channel_name.as_deref(),
                                                        };

                                                        if let Some(mut msg) =
                                                            build_message_from_post(&post, &message_context)
                                                        {
                                                            // Detect thread replies to the bot:
                                                            // if root_id is set, fetch the root post
                                                            // and check if the bot authored it.
                                                            if !post.root_id.is_empty()
                                                                && let Some(root_author) = resolve_root_post_author(
                                                                    &ws_client,
                                                                    token.as_ref(),
                                                                    &ws_base_url,
                                                                    &post.root_id,
                                                                ).await
                                                                && root_author == bot_user_id.as_ref()
                                                            {
                                                                msg.metadata.insert(
                                                                    "mattermost_mentions_or_replies_to_bot".into(),
                                                                    serde_json::json!(true),
                                                                );
                                                            }
                                                            if inbound_tx_clone.send(msg).await.is_err() {
                                                                tracing::debug!("inbound channel closed");
                                                                return;
                                                            }
                                                        }
                                                    }
                                                }
                                            } // close Ok(event) arm
                                            } // close match MattermostWsEvent
                                        }
                                        Some(Ok(WsMessage::Ping(data))) => {
                                            match write.send(WsMessage::Pong(data.clone())).await {
                                                Ok(()) => {}
                                                Err(_) => {
                                                    tracing::warn!(adapter = %runtime_key, "failed to send pong");
                                                    break;
                                                }
                                            }
                                        }
                                        Some(Ok(WsMessage::Pong(_))) => {}
                                        Some(Ok(WsMessage::Close(_))) => {
                                            tracing::info!(adapter = %runtime_key, "websocket closed by server");
                                            break;
                                        }
                                        Some(Err(error)) => {
                                            tracing::error!(adapter = %runtime_key, %error, "websocket error");
                                            break;
                                        }
                                        None => break,
                                        _ => {}
                                    }
                                }
                            }
                        }

                        tracing::info!(adapter = %runtime_key, "websocket disconnected, reconnecting...");
                    }
                    Err(error) => {
                        tracing::error!(
                            adapter = %runtime_key,
                            %error,
                            delay_ms = retry_delay.as_millis(),
                            "websocket connection failed, retrying"
                        );
                    }
                }

                tokio::select! {
                    _ = tokio::time::sleep(retry_delay) => {
                        retry_delay = (retry_delay * 2).min(WS_RECONNECT_MAX_DELAY);
                    }
                    _ = shutdown_rx.recv() => {
                        tracing::info!(adapter = %runtime_key, "mattermost adapter shutting down during reconnect delay");
                        return;
                    }
                }
            }
        });

        *self.ws_task.write().await = Some(handle);

        let stream = tokio_stream::wrappers::ReceiverStream::new(inbound_rx);
        Ok(Box::pin(stream))
    }

    async fn respond(
        &self,
        message: &InboundMessage,
        response: OutboundResponse,
    ) -> crate::Result<()> {
        let channel_id = self.extract_channel_id(message)?;

        match response {
            OutboundResponse::Text(text) => {
                self.stop_typing(channel_id).await;
                // Use root_id for threading: prefer mattermost_root_id (when triggered from a
                // threaded message) or REPLY_TO_MESSAGE_ID (set by channel.rs for branch/worker
                // replies).
                let root_id = message
                    .metadata
                    .get("mattermost_root_id")
                    .and_then(|v| v.as_str())
                    .or_else(|| {
                        message
                            .metadata
                            .get(crate::metadata_keys::REPLY_TO_MESSAGE_ID)
                            .and_then(|v| v.as_str())
                    });

                for chunk in split_message(&text, MAX_MESSAGE_LENGTH) {
                    self.create_post(channel_id, &chunk, root_id).await?;
                }
            }

            OutboundResponse::StreamStart => {
                let root_id = message
                    .metadata
                    .get("mattermost_root_id")
                    .and_then(|v| v.as_str())
                    .or_else(|| {
                        message
                            .metadata
                            .get(crate::metadata_keys::REPLY_TO_MESSAGE_ID)
                            .and_then(|v| v.as_str())
                    });
                self.start_typing(channel_id).await;
                // Create a placeholder post with a zero-width space.
                let post = match self.create_post(channel_id, "\u{200B}", root_id).await {
                    Ok(p) => p,
                    Err(error) => {
                        self.stop_typing(channel_id).await;
                        return Err(error);
                    }
                };
                self.active_messages.write().await.insert(
                    message.id.clone(),
                    ActiveStream {
                        post_id: post.id.into(),
                        channel_id: channel_id.to_string().into(),
                        last_edit: Instant::now(),
                        accumulated_text: String::new(),
                    },
                );
            }

            OutboundResponse::StreamChunk(chunk) => {
                let pending_edit = {
                    let mut active_messages = self.active_messages.write().await;
                    if let Some(active) = active_messages.get_mut(&message.id) {
                        active.accumulated_text.push_str(&chunk);

                        if active.last_edit.elapsed() > STREAM_EDIT_THROTTLE {
                            let display_text = if active.accumulated_text.len() > MAX_MESSAGE_LENGTH
                            {
                                let end = active
                                    .accumulated_text
                                    .floor_char_boundary(MAX_MESSAGE_LENGTH - 3);
                                format!("{}...", &active.accumulated_text[..end])
                            } else {
                                active.accumulated_text.clone()
                            };
                            active.last_edit = Instant::now();
                            Some((active.post_id.clone(), display_text))
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                };
                if let Some((post_id, display_text)) = pending_edit
                    && let Err(error) = self.edit_post(&post_id, &display_text).await
                {
                    tracing::warn!(%error, "failed to edit streaming message");
                }
            }

            OutboundResponse::StreamEnd => {
                self.stop_typing(channel_id).await;
                let root_id = message
                    .metadata
                    .get("mattermost_root_id")
                    .and_then(|v| v.as_str())
                    .or_else(|| {
                        message
                            .metadata
                            .get(crate::metadata_keys::REPLY_TO_MESSAGE_ID)
                            .and_then(|v| v.as_str())
                    });
                if let Some(active) = self.active_messages.write().await.remove(&message.id) {
                    let chunks = split_message(&active.accumulated_text, MAX_MESSAGE_LENGTH);
                    let mut first = true;
                    for chunk in chunks {
                        if first {
                            first = false;
                            if let Err(error) = self.edit_post(&active.post_id, &chunk).await {
                                tracing::warn!(%error, "failed to finalize streaming message");
                            }
                        } else if let Err(error) =
                            self.create_post(channel_id, &chunk, root_id).await
                        {
                            tracing::warn!(%error, "failed to create overflow chunk for streaming message");
                        }
                    }
                }
            }

            OutboundResponse::Status(status) => self.send_status(message, status).await?,

            OutboundResponse::Reaction(emoji) => {
                let post_id = message
                    .metadata
                    .get("mattermost_post_id")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow::anyhow!("missing mattermost_post_id metadata"))?;
                let emoji_name = sanitize_reaction_name(&emoji);

                let bot_user_id = self
                    .bot_user_id
                    .get()
                    .ok_or_else(|| {
                        anyhow::anyhow!("bot_user_id not initialized; call start() first")
                    })?
                    .as_ref()
                    .to_string();

                let response = self
                    .client
                    .post(self.api_url("/reactions"))
                    .bearer_auth(self.token.as_ref())
                    .json(&serde_json::json!({
                        "user_id": bot_user_id,
                        "post_id": post_id,
                        "emoji_name": emoji_name,
                    }))
                    .send()
                    .await
                    .context("failed to add reaction")?;

                if !response.status().is_success() {
                    tracing::warn!(
                        status = %response.status(),
                        emoji = %emoji_name,
                        "failed to add reaction"
                    );
                }
            }

            OutboundResponse::File {
                filename,
                data,
                mime_type,
                caption,
            } => {
                if data.len() > self.max_attachment_bytes {
                    return Err(anyhow::anyhow!(
                        "file too large: {} bytes (max: {})",
                        data.len(),
                        self.max_attachment_bytes
                    )
                    .into());
                }

                let part = reqwest::multipart::Part::bytes(data)
                    .file_name(filename.clone())
                    .mime_str(&mime_type)
                    .context("invalid mime type")?;

                let form = reqwest::multipart::Form::new()
                    .part("files", part)
                    .text("channel_id", channel_id.to_string());

                let response = self
                    .client
                    .post(self.api_url("/files"))
                    .bearer_auth(self.token.as_ref())
                    .multipart(form)
                    .send()
                    .await
                    .context("failed to upload file")?;

                if !response.status().is_success() {
                    let body = response.text().await.unwrap_or_default();
                    return Err(anyhow::anyhow!("mattermost file upload failed: {body}").into());
                }

                let upload: MattermostFileUpload = response
                    .json()
                    .await
                    .context("failed to parse file upload response")?;

                let file_ids: Vec<_> = upload.file_infos.iter().map(|f| f.id.as_str()).collect();
                let root_id = message
                    .metadata
                    .get("mattermost_root_id")
                    .and_then(|v| v.as_str())
                    .or_else(|| {
                        message
                            .metadata
                            .get(crate::metadata_keys::REPLY_TO_MESSAGE_ID)
                            .and_then(|v| v.as_str())
                    });
                let post_response = self
                    .client
                    .post(self.api_url("/posts"))
                    .bearer_auth(self.token.as_ref())
                    .json(&serde_json::json!({
                        "channel_id": channel_id,
                        "message": caption.unwrap_or_default(),
                        "file_ids": file_ids,
                        "root_id": root_id.unwrap_or(""),
                    }))
                    .send()
                    .await
                    .context("failed to create post with file")?;
                let post_status = post_response.status();
                if !post_status.is_success() {
                    let body = post_response.text().await.unwrap_or_default();
                    return Err(anyhow::anyhow!(
                        "mattermost POST /posts (file) failed with status {}: {body}",
                        post_status.as_u16()
                    )
                    .into());
                }
            }

            _ => {
                tracing::debug!(
                    ?response,
                    "mattermost adapter does not support this response type"
                );
            }
        }

        Ok(())
    }

    async fn send_status(
        &self,
        message: &InboundMessage,
        status: StatusUpdate,
    ) -> crate::Result<()> {
        let channel_id = self.extract_channel_id(message)?;

        match status {
            StatusUpdate::Thinking => {
                self.start_typing(channel_id).await;
            }
            StatusUpdate::StopTyping => {
                self.stop_typing(channel_id).await;
            }
            _ => {}
        }

        Ok(())
    }

    async fn fetch_history(
        &self,
        message: &InboundMessage,
        limit: usize,
    ) -> crate::Result<Vec<HistoryMessage>> {
        let channel_id = self.extract_channel_id(message)?;
        let before_post_id = message
            .metadata
            .get("mattermost_post_id")
            .and_then(|v| v.as_str());

        let capped_limit = limit.min(200) as u32;
        let posts = self
            .get_channel_posts(channel_id, before_post_id, capped_limit)
            .await?;

        let mut posts_vec: Vec<_> = posts.posts.into_values().collect();
        posts_vec.sort_by_key(|p| p.create_at);

        let bot_id = self.bot_user_id.get().map(|user_id| user_id.as_ref());
        let history: Vec<HistoryMessage> = posts_vec
            .into_iter()
            .map(|post| {
                let is_bot = bot_id == Some(post.user_id.as_str());
                HistoryMessage {
                    author: if is_bot {
                        "bot".to_string()
                    } else {
                        post.user_id
                    },
                    content: post.message,
                    is_bot,
                    timestamp: chrono::DateTime::from_timestamp_millis(post.create_at),
                }
            })
            .collect();

        Ok(history)
    }

    async fn health_check(&self) -> crate::Result<()> {
        let response = self
            .client
            .get(self.api_url("/system/ping"))
            .bearer_auth(self.token.as_ref())
            .send()
            .await
            .context("health check request failed")?;

        if !response.status().is_success() {
            return Err(anyhow::anyhow!(
                "mattermost health check failed: status {}",
                response.status()
            )
            .into());
        }

        Ok(())
    }

    async fn shutdown(&self) -> crate::Result<()> {
        if let Some(tx) = self.shutdown_tx.write().await.take() {
            tx.send(()).await.ok();
        }

        if let Some(handle) = self.ws_task.write().await.take() {
            handle.abort();
        }

        for (_, handle) in self.typing_tasks.write().await.drain() {
            handle.abort();
        }
        self.active_messages.write().await.clear();

        tracing::info!(adapter = %self.runtime_key, "mattermost adapter shut down");
        Ok(())
    }

    async fn broadcast(&self, target: &str, response: OutboundResponse) -> crate::Result<()> {
        // Resolve DM targets (dm:{user_id}) to a real Mattermost channel ID.
        let resolved_target;
        let target = if let Some(user_id) = target.strip_prefix("dm:") {
            resolved_target = self.get_or_create_dm_channel(user_id).await?;
            resolved_target.as_str()
        } else {
            target
        };

        match response {
            OutboundResponse::Text(text) => {
                for chunk in split_message(&text, MAX_MESSAGE_LENGTH) {
                    self.create_post(target, &chunk, None).await?;
                }
            }
            OutboundResponse::File {
                filename,
                data,
                mime_type,
                caption,
            } => {
                if data.len() > self.max_attachment_bytes {
                    return Err(anyhow::anyhow!(
                        "file too large: {} bytes (max: {})",
                        data.len(),
                        self.max_attachment_bytes
                    )
                    .into());
                }
                let part = reqwest::multipart::Part::bytes(data)
                    .file_name(filename)
                    .mime_str(&mime_type)
                    .context("invalid mime type")?;
                let form = reqwest::multipart::Form::new()
                    .part("files", part)
                    .text("channel_id", target.to_string());
                let upload_response = self
                    .client
                    .post(self.api_url("/files"))
                    .bearer_auth(self.token.as_ref())
                    .multipart(form)
                    .send()
                    .await
                    .context("failed to upload file")?;
                let upload_status = upload_response.status();
                if !upload_status.is_success() {
                    let body = upload_response.text().await.unwrap_or_default();
                    return Err(anyhow::anyhow!(
                        "mattermost file upload failed with status {}: {body}",
                        upload_status.as_u16()
                    )
                    .into());
                }
                let upload: MattermostFileUpload = upload_response
                    .json()
                    .await
                    .context("failed to parse file upload response")?;
                let file_ids: Vec<_> = upload.file_infos.iter().map(|f| f.id.as_str()).collect();
                let post_response = self
                    .client
                    .post(self.api_url("/posts"))
                    .bearer_auth(self.token.as_ref())
                    .json(&serde_json::json!({
                        "channel_id": target,
                        "message": caption.unwrap_or_default(),
                        "file_ids": file_ids,
                    }))
                    .send()
                    .await
                    .context("failed to create post with file")?;
                let post_status = post_response.status();
                if !post_status.is_success() {
                    let body = post_response.text().await.unwrap_or_default();
                    return Err(anyhow::anyhow!(
                        "mattermost create post with file failed with status {}: {body}",
                        post_status.as_u16()
                    )
                    .into());
                }
            }
            other => {
                tracing::debug!(
                    ?other,
                    "mattermost broadcast does not support this response type"
                );
            }
        }
        Ok(())
    }
}

/// Convert a [`MattermostPost`] from a WebSocket event into an [`InboundMessage`],
/// applying all permission filters.
///
/// Returns `None` (message dropped) when any of the following hold:
/// - The post was authored by the bot itself.
/// - A `team_filter` is configured and the event's team ID does not match, or
///   the team ID is absent (fail-closed).
/// - A `channel_filter` is configured and the channel is not in the allow-list
///   for the event's team, or the team ID is absent (fail-closed).
/// - The channel is a direct message (`channel_type = "D"`) and either
///   `dm_allowed_users` is empty or the sender is not listed.
///
/// When a message passes all filters the following metadata keys are set:
/// `message_id`, `mattermost_post_id`, `mattermost_channel_id`,
/// `mattermost_team_id` (if known), `mattermost_root_id` (if in a thread),
/// `sender_display_name` (if `display_name` is provided),
/// `mattermost_channel_name` and `channel_name` (if `channel_name` is provided),
/// and `mattermost_mentions_or_replies_to_bot` (true when the message text
/// contains `@{bot_username}`).
fn build_message_from_post(
    post: &MattermostPost,
    context: &MessageBuildContext<'_>,
) -> Option<InboundMessage> {
    if post.user_id == context.bot_user_id {
        return None;
    }

    if let Some(team_filter) = &context.permissions.team_filter {
        // Fail-closed: no team_id in the event → can't verify team → reject.
        let Some(tid) = context.team_id else {
            return None;
        };
        if !team_filter.contains(tid) {
            return None;
        }
    }

    if !context.permissions.channel_filter.is_empty() {
        // Fail-closed: no team_id or no allowlist entry for this team → reject.
        let Some(tid) = context.team_id else {
            return None;
        };
        let allowed_channels = context.permissions.channel_filter.get(tid)?;
        if !allowed_channels.contains(&post.channel_id) {
            return None;
        }
    }

    // DM filter: if channel_type is "D", enforce dm_allowed_users (fail-closed)
    if post.channel_type.as_deref() == Some("D") {
        if context.permissions.dm_allowed_users.is_empty() {
            return None;
        }
        if !context.permissions.dm_allowed_users.contains(&post.user_id) {
            return None;
        }
    }

    // "D" = direct message, "G" = group DM
    let conversation_id = if post.channel_type.as_deref() == Some("D") {
        apply_runtime_adapter_to_conversation_id(
            context.runtime_key,
            format!(
                "mattermost:{}:dm:{}",
                context.team_id.as_deref().unwrap_or(""),
                post.user_id
            ),
        )
    } else {
        apply_runtime_adapter_to_conversation_id(
            context.runtime_key,
            format!(
                "mattermost:{}:{}",
                context.team_id.as_deref().unwrap_or(""),
                post.channel_id
            ),
        )
    };

    let mut metadata = HashMap::new();

    metadata.insert(
        crate::metadata_keys::MESSAGE_ID.into(),
        serde_json::json!(&post.id),
    );

    metadata.insert("mattermost_post_id".into(), serde_json::json!(&post.id));
    metadata.insert(
        "mattermost_channel_id".into(),
        serde_json::json!(&post.channel_id),
    );
    if let Some(tid) = context.team_id {
        metadata.insert("mattermost_team_id".into(), serde_json::json!(tid));
    }
    if !post.root_id.is_empty() {
        metadata.insert(
            "mattermost_root_id".into(),
            serde_json::json!(&post.root_id),
        );
    }

    // FN1: sender display name
    if let Some(dn) = context.display_name {
        metadata.insert("sender_display_name".into(), serde_json::json!(dn));
    }

    // FN2: channel name
    if let Some(cn) = context.channel_name {
        metadata.insert("mattermost_channel_name".into(), serde_json::json!(cn));
        metadata.insert(
            crate::metadata_keys::CHANNEL_NAME.into(),
            serde_json::json!(cn),
        );
    }

    // FN4: bot mention detection — @mention, DM, or thread reply to bot post.
    // Thread-reply-to-bot detection is handled asynchronously in the WS event
    // handler and may upgrade this to true after this function returns.
    let is_dm = post.channel_type.as_deref() == Some("D");
    let mentions_bot = is_dm
        || (!context.bot_username.is_empty()
            && post.message.contains(&format!("@{}", context.bot_username)));
    metadata.insert(
        "mattermost_mentions_or_replies_to_bot".into(),
        serde_json::json!(mentions_bot),
    );

    // FN1: formatted_author — "Display Name" when display name is available
    let formatted_author = context.display_name.map(|dn| dn.to_string());

    Some(InboundMessage {
        id: post.id.clone(),
        source: "mattermost".into(),
        adapter: Some(context.runtime_key.to_string()),
        conversation_id,
        sender_id: post.user_id.clone(),
        agent_id: None,
        content: MessageContent::Text(post.message.clone()),
        timestamp: chrono::DateTime::from_timestamp_millis(post.create_at)
            .unwrap_or_else(chrono::Utc::now),
        metadata,
        formatted_author,
    })
}

// --- API Types ---

#[derive(Debug, Clone, Deserialize)]
struct MattermostUser {
    id: String,
    username: String,
    #[serde(default)]
    first_name: String,
    #[serde(default)]
    last_name: String,
    #[serde(default)]
    nickname: String,
}

impl MattermostUser {
    /// Resolve the best available display name for this user.
    ///
    /// Priority order: nickname → "first last" (trimmed) → username.
    fn display_name(&self) -> String {
        if !self.nickname.is_empty() {
            return self.nickname.clone();
        }
        let full = format!("{} {}", self.first_name, self.last_name);
        let full = full.trim();
        if !full.is_empty() {
            return full.to_string();
        }
        self.username.clone()
    }
}

#[derive(Debug, Deserialize)]
struct MattermostChannel {
    id: String,
    display_name: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct MattermostPost {
    id: String,
    create_at: i64,
    #[allow(dead_code)]
    update_at: i64,
    user_id: String,
    channel_id: String,
    root_id: String,
    message: String,
    /// "D" = direct message, "G" = group DM, "O" = public, "P" = private.
    /// Not present in REST list responses; injected from WS event data.
    #[serde(default)]
    channel_type: Option<String>,
    #[serde(default)]
    #[allow(dead_code)]
    file_ids: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct MattermostPostList {
    #[serde(default)]
    #[allow(dead_code)]
    order: Vec<String>,
    #[serde(default)]
    posts: HashMap<String, MattermostPost>,
}

#[derive(Debug, Deserialize)]
struct MattermostFileUpload {
    #[serde(default)]
    file_infos: Vec<MattermostFileInfo>,
}

#[derive(Debug, Deserialize)]
struct MattermostFileInfo {
    id: String,
    #[allow(dead_code)]
    name: String,
}

#[derive(Debug, Deserialize)]
struct MattermostWsEvent {
    event: String,
    #[serde(default)]
    data: serde_json::Value,
    #[serde(default)]
    broadcast: MattermostWsBroadcast,
}

#[derive(Debug, Deserialize, Default)]
struct MattermostWsBroadcast {
    #[serde(default)]
    #[allow(dead_code)]
    channel_id: Option<String>,
    #[serde(default)]
    team_id: Option<String>,
    #[serde(default)]
    #[allow(dead_code)]
    user_id: Option<String>,
}

/// Look up the display name for a Mattermost user by ID.
///
/// Returns the cached value if already resolved, otherwise calls
/// `GET /api/v4/users/{user_id}` and caches the result. The resolved name
/// follows the same priority order as [`MattermostUser::display_name`]:
/// nickname → "first last" → username. Returns `None` on any network or
/// API error (logged at `DEBUG` level).
async fn resolve_user_display_name(
    cache: &RwLock<HashMap<String, String>>,
    client: &Client,
    token: &str,
    base_url: &Url,
    user_id: &str,
) -> Option<String> {
    if let Some(name) = cache.read().await.get(user_id).cloned() {
        return Some(name);
    }
    let mut url = base_url.clone();
    url.path_segments_mut()
        .ok()?
        .extend(["api", "v4", "users", user_id]);
    let resp = match client.get(url).bearer_auth(token).send().await {
        Ok(r) => r,
        Err(error) => {
            tracing::debug!(%error, user_id, "failed to fetch mattermost user");
            return None;
        }
    };
    if !resp.status().is_success() {
        tracing::debug!(status = %resp.status(), user_id, "mattermost user fetch returned non-success");
        return None;
    }
    let user: MattermostUser = match resp.json().await {
        Ok(u) => u,
        Err(error) => {
            tracing::debug!(%error, user_id, "failed to parse mattermost user response");
            return None;
        }
    };
    let name = user.display_name();
    cache
        .write()
        .await
        .insert(user_id.to_string(), name.clone());
    Some(name)
}

/// Look up the display name for a Mattermost channel by ID.
///
/// Returns the cached value if already resolved, otherwise calls
/// `GET /api/v4/channels/{channel_id}` and caches the result using the
/// channel's `display_name` field. Returns `None` on any network or API
/// error (logged at `DEBUG` level).
async fn resolve_channel_name(
    cache: &RwLock<HashMap<String, String>>,
    client: &Client,
    token: &str,
    base_url: &Url,
    channel_id: &str,
) -> Option<String> {
    if let Some(name) = cache.read().await.get(channel_id).cloned() {
        return Some(name);
    }
    let mut url = base_url.clone();
    url.path_segments_mut()
        .ok()?
        .extend(["api", "v4", "channels", channel_id]);
    let resp = match client.get(url).bearer_auth(token).send().await {
        Ok(r) => r,
        Err(error) => {
            tracing::debug!(%error, channel_id, "failed to fetch mattermost channel");
            return None;
        }
    };
    if !resp.status().is_success() {
        tracing::debug!(status = %resp.status(), channel_id, "mattermost channel fetch returned non-success");
        return None;
    }
    let channel: MattermostChannel = match resp.json().await {
        Ok(c) => c,
        Err(error) => {
            tracing::debug!(%error, channel_id, "failed to parse mattermost channel response");
            return None;
        }
    };
    let name = channel.display_name;
    cache
        .write()
        .await
        .insert(channel_id.to_string(), name.clone());
    Some(name)
}

/// Fetch the `user_id` of the author of a Mattermost post by its ID.
///
/// Used to detect thread replies to the bot: if a post's `root_id` resolves to
/// a post authored by the bot, the reply should be treated as directed at the
/// bot for `require_mention` purposes. Returns `None` on any network or API
/// error (logged at `DEBUG` level).
async fn resolve_root_post_author(
    client: &Client,
    token: &str,
    base_url: &Url,
    post_id: &str,
) -> Option<String> {
    let mut url = base_url.clone();
    url.path_segments_mut()
        .ok()?
        .extend(["api", "v4", "posts", post_id]);
    let resp = match client.get(url).bearer_auth(token).send().await {
        Ok(r) => r,
        Err(error) => {
            tracing::debug!(%error, post_id, "failed to fetch mattermost root post");
            return None;
        }
    };
    if !resp.status().is_success() {
        tracing::debug!(status = %resp.status(), post_id, "mattermost root post fetch returned non-success");
        return None;
    }
    let post: MattermostPost = match resp.json().await {
        Ok(p) => p,
        Err(error) => {
            tracing::debug!(%error, post_id, "failed to parse mattermost root post response");
            return None;
        }
    };
    Some(post.user_id)
}

/// Convert an emoji input to a Mattermost reaction short-code name.
///
/// Handles three input forms:
/// 1. Unicode emoji (e.g. "👍") → looked up via the `emojis` crate → "thumbsup"
/// 2. Colon-wrapped short-code (e.g. ":thumbsup:") → stripped to "thumbsup"
/// 3. Plain short-code (e.g. "thumbsup") → lowercased and passed through
fn sanitize_reaction_name(emoji: &str) -> String {
    let trimmed = emoji.trim();
    if let Some(e) = emojis::get(trimmed) {
        if let Some(shortcode) = e.shortcode() {
            return shortcode.to_string();
        }
        return e.name().replace(' ', "_").to_lowercase();
    }
    trimmed
        .trim_start_matches(':')
        .trim_end_matches(':')
        .to_lowercase()
}

/// Split `text` into chunks no longer than `max_len` bytes.
///
/// Splits preferentially on newlines, then on spaces, and falls back to a
/// hard character-boundary break when no whitespace is found. All splits
/// respect UTF-8 character boundaries. Leading whitespace is stripped from
/// each chunk after a split.
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

        let search_end = remaining.floor_char_boundary(max_len);
        let search_region = &remaining[..search_end];
        // Ignore a break point at position 0 to avoid pushing an empty chunk
        // (e.g. when the region starts with a newline or space).
        let break_point = search_region
            .rfind('\n')
            .or_else(|| search_region.rfind(' '))
            .filter(|&pos| pos > 0)
            .unwrap_or(search_end);

        let end = remaining.floor_char_boundary(break_point);
        chunks.push(remaining[..end].to_string());
        remaining = remaining[end..].trim_start_matches('\n').trim_start();
    }

    chunks
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- helpers ---

    fn post(user_id: &str, channel_id: &str, channel_type: Option<&str>) -> MattermostPost {
        MattermostPost {
            id: "post1".into(),
            create_at: 0,
            update_at: 0,
            user_id: user_id.into(),
            channel_id: channel_id.into(),
            root_id: String::new(),
            message: "hello".into(),
            channel_type: channel_type.map(String::from),
            file_ids: vec![],
        }
    }

    fn no_filters() -> MattermostPermissions {
        MattermostPermissions {
            team_filter: None,
            channel_filter: HashMap::new(),
            dm_allowed_users: vec![],
        }
    }

    fn build_message_from_mattermost_post(
        post: &MattermostPost,
        bot_id: &str,
        team_id: Option<&str>,
        perms: &MattermostPermissions,
    ) -> Option<InboundMessage> {
        let team_id = team_id.map(String::from);
        let context = MessageBuildContext {
            runtime_key: "mattermost",
            bot_user_id: bot_id,
            bot_username: "botuser",
            team_id: &team_id,
            permissions: perms,
            display_name: None,
            channel_name: None,
        };
        build_message_from_post(post, &context)
    }

    fn build_message_from_mattermost_post_named(
        post: &MattermostPost,
        bot_id: &str,
        bot_username: &str,
        team_id: Option<&str>,
        perms: &MattermostPermissions,
        display_name: Option<&str>,
        channel_name: Option<&str>,
    ) -> Option<InboundMessage> {
        let team_id = team_id.map(String::from);
        let context = MessageBuildContext {
            runtime_key: "mattermost",
            bot_user_id: bot_id,
            bot_username,
            team_id: &team_id,
            permissions: perms,
            display_name,
            channel_name,
        };
        build_message_from_post(post, &context)
    }

    // --- build_message_from_post ---

    #[test]
    fn bot_messages_are_filtered() {
        let p = post("bot123", "chan1", None);
        assert!(build_message_from_mattermost_post(&p, "bot123", None, &no_filters()).is_none());
    }

    #[test]
    fn non_bot_message_passes_without_filters() {
        let p = post("user1", "chan1", None);
        assert!(
            build_message_from_mattermost_post(&p, "bot123", Some("team1"), &no_filters())
                .is_some()
        );
    }

    #[test]
    fn team_filter_allows_matching_team() {
        let p = post("user1", "chan1", None);
        let perms = MattermostPermissions {
            team_filter: Some(vec!["team1".into()]),
            channel_filter: HashMap::new(),
            dm_allowed_users: vec![],
        };
        assert!(build_message_from_mattermost_post(&p, "bot", Some("team1"), &perms).is_some());
    }

    #[test]
    fn team_filter_rejects_wrong_team() {
        let p = post("user1", "chan1", None);
        let perms = MattermostPermissions {
            team_filter: Some(vec!["team1".into()]),
            channel_filter: HashMap::new(),
            dm_allowed_users: vec![],
        };
        assert!(build_message_from_mattermost_post(&p, "bot", Some("team2"), &perms).is_none());
    }

    #[test]
    fn team_filter_fail_closed_when_team_id_absent() {
        let p = post("user1", "chan1", None);
        let perms = MattermostPermissions {
            team_filter: Some(vec!["team1".into()]),
            channel_filter: HashMap::new(),
            dm_allowed_users: vec![],
        };
        // No team_id in the event — must reject (fail-closed)
        assert!(build_message_from_mattermost_post(&p, "bot", None, &perms).is_none());
    }

    #[test]
    fn channel_filter_allows_matching_channel() {
        let p = post("user1", "chan1", None);
        let mut cf = HashMap::new();
        cf.insert("team1".into(), vec!["chan1".into()]);
        let perms = MattermostPermissions {
            team_filter: None,
            channel_filter: cf,
            dm_allowed_users: vec![],
        };
        assert!(build_message_from_mattermost_post(&p, "bot", Some("team1"), &perms).is_some());
    }

    #[test]
    fn channel_filter_rejects_unlisted_channel() {
        let p = post("user1", "chan2", None);
        let mut cf = HashMap::new();
        cf.insert("team1".into(), vec!["chan1".into()]);
        let perms = MattermostPermissions {
            team_filter: None,
            channel_filter: cf,
            dm_allowed_users: vec![],
        };
        assert!(build_message_from_mattermost_post(&p, "bot", Some("team1"), &perms).is_none());
    }

    #[test]
    fn channel_filter_fail_closed_when_team_id_absent() {
        let p = post("user1", "chan1", None);
        let mut cf = HashMap::new();
        cf.insert("team1".into(), vec!["chan1".into()]);
        let perms = MattermostPermissions {
            team_filter: None,
            channel_filter: cf,
            dm_allowed_users: vec![],
        };
        // No team_id → can't look up allowed channels → reject
        assert!(build_message_from_mattermost_post(&p, "bot", None, &perms).is_none());
    }

    #[test]
    fn channel_filter_fail_closed_when_team_not_in_filter() {
        // channel_filter has an entry for team1 but the message comes from team2.
        // Even though chan1 is the allowed channel, the missing team2 entry must
        // reject (fail-closed), not silently pass through.
        let p = post("user1", "chan1", None);
        let mut cf = HashMap::new();
        cf.insert("team1".into(), vec!["chan1".into()]);
        let perms = MattermostPermissions {
            team_filter: None,
            channel_filter: cf,
            dm_allowed_users: vec![],
        };
        assert!(build_message_from_mattermost_post(&p, "bot", Some("team2"), &perms).is_none());
    }

    fn dm_perms(allowed: &[&str]) -> MattermostPermissions {
        MattermostPermissions {
            team_filter: None,
            channel_filter: HashMap::new(),
            dm_allowed_users: allowed.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn dm_blocked_when_dm_allowed_users_empty() {
        let p = post("user1", "chan1", Some("D"));
        assert!(
            build_message_from_mattermost_post(&p, "bot", Some("team1"), &no_filters()).is_none()
        );
    }

    #[test]
    fn dm_allowed_for_listed_user() {
        let p = post("user1", "chan1", Some("D"));
        assert!(
            build_message_from_mattermost_post(&p, "bot", Some("team1"), &dm_perms(&["user1"]))
                .is_some()
        );
    }

    #[test]
    fn dm_blocked_for_unlisted_user() {
        let p = post("user2", "chan1", Some("D"));
        assert!(
            build_message_from_mattermost_post(&p, "bot", Some("team1"), &dm_perms(&["user1"]))
                .is_none()
        );
    }

    #[test]
    fn dm_filter_does_not_affect_channel_messages() {
        // channel messages (type "O") pass even with empty dm_allowed_users
        let p = post("user1", "chan1", Some("O"));
        assert!(
            build_message_from_mattermost_post(&p, "bot", Some("team1"), &no_filters()).is_some()
        );
    }

    #[test]
    fn dm_conversation_id_uses_user_id() {
        let p = post("user1", "chan1", Some("D"));
        let msg =
            build_message_from_mattermost_post(&p, "bot", Some("team1"), &dm_perms(&["user1"]))
                .unwrap();
        assert!(
            msg.conversation_id.contains(":dm:user1"),
            "expected DM conversation_id, got {}",
            msg.conversation_id
        );
    }

    #[test]
    fn channel_conversation_id_uses_channel_id() {
        let p = post("user1", "chan1", Some("O"));
        let msg =
            build_message_from_mattermost_post(&p, "bot", Some("team1"), &no_filters()).unwrap();
        assert!(
            msg.conversation_id.contains(":chan1"),
            "expected channel conversation_id, got {}",
            msg.conversation_id
        );
        assert!(
            !msg.conversation_id.contains(":dm:"),
            "should not be DM, got {}",
            msg.conversation_id
        );
    }

    #[test]
    fn message_id_metadata_is_set() {
        let p = post("user1", "chan1", None);
        let msg =
            build_message_from_mattermost_post(&p, "bot", Some("team1"), &no_filters()).unwrap();
        assert!(msg.metadata.contains_key(crate::metadata_keys::MESSAGE_ID));
    }

    // --- FN4: bot mention detection ---

    #[test]
    fn mention_sets_flag_when_at_bot_username_in_message() {
        let mut p = post("user1", "chan1", Some("O"));
        p.message = "hey @botuser can you help?".into();
        let msg =
            build_message_from_mattermost_post(&p, "bot", Some("team1"), &no_filters()).unwrap();
        assert_eq!(
            msg.metadata
                .get("mattermost_mentions_or_replies_to_bot")
                .and_then(|v| v.as_bool()),
            Some(true),
        );
    }

    #[test]
    fn no_mention_flag_when_bot_not_mentioned() {
        let p = post("user1", "chan1", Some("O"));
        let msg =
            build_message_from_mattermost_post(&p, "bot", Some("team1"), &no_filters()).unwrap();
        assert_eq!(
            msg.metadata
                .get("mattermost_mentions_or_replies_to_bot")
                .and_then(|v| v.as_bool()),
            Some(false),
        );
    }

    // --- FN1: display name / sender_display_name ---

    #[test]
    fn sender_display_name_set_when_provided() {
        let p = post("user1", "chan1", Some("O"));
        let msg = build_message_from_mattermost_post_named(
            &p,
            "bot",
            "botuser",
            Some("team1"),
            &no_filters(),
            Some("Alice"),
            None,
        )
        .unwrap();
        assert_eq!(
            msg.metadata
                .get("sender_display_name")
                .and_then(|v| v.as_str()),
            Some("Alice"),
        );
        assert_eq!(msg.formatted_author.as_deref(), Some("Alice"));
    }

    #[test]
    fn sender_display_name_absent_when_not_provided() {
        let p = post("user1", "chan1", Some("O"));
        let msg =
            build_message_from_mattermost_post(&p, "bot", Some("team1"), &no_filters()).unwrap();
        assert!(!msg.metadata.contains_key("sender_display_name"));
        assert!(msg.formatted_author.is_none());
    }

    // --- FN2: channel name ---

    #[test]
    fn channel_name_metadata_set_when_provided() {
        let p = post("user1", "chan1", Some("O"));
        let msg = build_message_from_mattermost_post_named(
            &p,
            "bot",
            "botuser",
            Some("team1"),
            &no_filters(),
            None,
            Some("general"),
        )
        .unwrap();
        assert_eq!(
            msg.metadata
                .get("mattermost_channel_name")
                .and_then(|v| v.as_str()),
            Some("general"),
        );
        assert_eq!(
            msg.metadata
                .get(crate::metadata_keys::CHANNEL_NAME)
                .and_then(|v| v.as_str()),
            Some("general"),
        );
    }

    // --- SEC2: sanitize_reaction_name ---

    #[test]
    fn sanitize_reaction_unicode_to_shortcode() {
        assert_eq!(sanitize_reaction_name("\u{1F44D}"), "+1");
    }

    #[test]
    fn sanitize_reaction_colon_wrapped() {
        assert_eq!(sanitize_reaction_name(":thumbsup:"), "thumbsup");
    }

    #[test]
    fn sanitize_reaction_plain_lowercased() {
        assert_eq!(sanitize_reaction_name("ThumbsUp"), "thumbsup");
    }

    // --- split_message ---

    #[test]
    fn test_split_message_short() {
        let result = split_message("hello", 100);
        assert_eq!(result, vec!["hello"]);
    }

    #[test]
    fn test_split_message_exact_boundary() {
        let text = "a".repeat(100);
        let result = split_message(&text, 100);
        assert_eq!(result.len(), 1);
    }

    #[test]
    fn test_split_message_on_newline() {
        let text = "line1\nline2\nline3";
        let result = split_message(text, 8);
        assert_eq!(result, vec!["line1", "line2", "line3"]);
    }

    #[test]
    fn test_split_message_on_space() {
        let text = "word1 word2 word3";
        let result = split_message(text, 12);
        assert_eq!(result, vec!["word1 word2", "word3"]);
    }

    #[test]
    fn test_split_message_forced_break() {
        let text = "abcdefghijklmnopqrstuvwxyz";
        let result = split_message(text, 10);
        assert_eq!(result, vec!["abcdefghij", "klmnopqrst", "uvwxyz"]);
    }
}
