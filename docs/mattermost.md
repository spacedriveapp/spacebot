# Mattermost Integration Plan

## Executive Summary

Add Mattermost messaging platform support to Spacebot following the existing adapter architecture. Implementation will use a custom HTTP + WebSocket client (not the `mattermost_api` crate) to maintain consistency with existing patterns and avoid limitations.

## Library Assessment: mattermost_api

The [mattermost_api](https://docs.rs/mattermost_api/latest/mattermost_api/) crate (v0.8.2) was evaluated and **rejected** for the following reasons:

### Critical Gaps

| Feature | Required | Crate Support |
|---------|----------|---------------|
| Edit post (streaming) | Yes | No |
| Channel history fetch | Yes | No |
| Typing indicator | Yes | No |
| Websocket disconnect | Yes | No (loops forever) |
| File upload | Medium | No |
| Reactions | Low | No |

### Style Conflicts

- Uses `#[async_trait]` macro - violates AGENTS.md guideline to use native RPITIT
- `WebsocketHandler` trait requires async_trait
- No control over websocket lifecycle

### Verdict

Build a minimal custom client using `reqwest` (HTTP) + `tokio-tungstenite` (WebSocket). This matches the pattern used by other adapters and gives us full control over the implementation.

---

## Adversarial Review

### Security Issues

| Issue | Severity | Mitigation |
|-------|----------|------------|
| Token stored in plain String | High | Use `DecryptedSecret` wrapper (see src/secrets/store.rs) |
| URL injection in api_url() | Medium | Validate base_url at config load time, use `url::Url` |
| Unvalidated post_id in edit_post | Medium | Validate alphanumeric format before interpolation |
| WebSocket URL derived from user input | Medium | Parse and validate URL, require https/wss in production |
| No HTTP response body limit | Medium | Set max response size on reqwest client |
| File upload size unbounded | Medium | Enforce max_attachment_bytes from config |

### Concurrency Bugs

| Issue | Severity | Mitigation |
|-------|----------|------------|
| Race between start() and respond() | High | Use `OnceCell` for bot_user_id, fail requests until initialized |
| StreamChunk lost if channel_id not in active_messages | Medium | Return error, let caller handle |
| WebSocket spawn detached, no join handle | High | Store `JoinHandle` in adapter, await in shutdown |
| typing_tasks abort without cleanup | Low | Acceptable - abort is correct behavior |
| split read/write without mutex | Medium | Use `futures::stream::SplitSink`/`SplitStream` pattern correctly |

### Maintainability Issues

| Issue | Severity | Mitigation |
|-------|----------|------------|
| Missing Debug impl on MattermostConfig | Medium | Add `#[derive(Debug)]` with token redaction |
| Missing Debug impl on MattermostAdapter | Medium | Implement Debug manually with redaction |
| Inline JSON builders | Low | Extract to typed request structs |
| "Full implementation omitted" in plan | High | Complete the InboundMessage building code |
| Magic numbers (500ms throttle) | Low | Extract to constants with comments |
| No reconnection logic | High | Implement exponential backoff reconnect |
| No rate limit handling | Medium | Parse 429 responses, implement backoff |

### Idiomatic Rust Issues

| Issue | Severity | Mitigation |
|-------|----------|------------|
| `impl Into<String>` for new() params | Low | Use `impl Into<Arc<str>>` or just `String` per style guide |
| Clone on large structs | Medium | Use `Arc<str>` for strings, avoid cloning |
| `Vec<String>` allocations in split_message | Low | Return `Cow<'_, str>` iterator or accept writer |
| Missing `#[non_exhaustive]` on config structs | Low | Add for future-proofing |
| Using `anyhow::Context` directly | Medium | Use crate's `Error` type for consistency |

---

## Architecture

### Message Flow

```
┌─────────────────────────────────────────────────────────────┐
│                    Mattermost Instance                       │
│  https://mattermost.example.com/api/v4/*                    │
└─────────────────┬───────────────────────────┬───────────────┘
                  │                           │
                  │ HTTP (REST API)           │ WebSocket
                  │                           │
                  ▼                           ▼
         ┌────────────────┐          ┌────────────────┐
         │  reqwest       │          │ tokio-         │
         │  client        │          │ tungstenite    │
         └───────┬────────┘          └───────┬────────┘
                  │                           │
                  └───────────┬───────────────┘
                              │
                              ▼
                   ┌────────────────────┐
                   │  MattermostAdapter │
                   │  (Messaging trait) │
                   └─────────┬──────────┘
                             │
                             ▼
                   ┌────────────────────┐
                   │  MessagingManager  │
                   │  (fan-in)          │
                   └────────────────────┘
```

### Conversation ID Format

```
mattermost:{team_id}:{channel_id}           # Public/private channel
mattermost:{team_id}:dm:{user_id}           # Direct message
mattermost:instance_name:{team_id}:{id}     # Named adapter instance
```

---

## Implementation

### 1. Configuration (src/config.rs)

#### Add to MessagingConfig

```rust
pub struct MessagingConfig {
    // ... existing fields
    pub mattermost: Option<MattermostConfig>,
}
```

#### New Config Structs

```rust
#[derive(Clone)]
pub struct MattermostConfig {
    pub enabled: bool,
    pub base_url: String,
    pub token: String,
    pub team_id: Option<String>,
    pub instances: Vec<MattermostInstanceConfig>,
    pub dm_allowed_users: Vec<String>,
    pub max_attachment_bytes: usize,
}

impl std::fmt::Debug for MattermostConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MattermostConfig")
            .field("enabled", &self.enabled)
            .field("base_url", &self.base_url)
            .field("token", &"[REDACTED]")
            .field("team_id", &self.team_id)
            .field("instances", &self.instances)
            .field("dm_allowed_users", &self.dm_allowed_users)
            .field("max_attachment_bytes", &self.max_attachment_bytes)
            .finish()
    }
}

#[derive(Clone)]
pub struct MattermostInstanceConfig {
    pub name: String,
    pub enabled: bool,
    pub base_url: String,
    pub token: String,
    pub team_id: Option<String>,
    pub dm_allowed_users: Vec<String>,
    pub max_attachment_bytes: usize,
}

impl std::fmt::Debug for MattermostInstanceConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MattermostInstanceConfig")
            .field("name", &self.name)
            .field("enabled", &self.enabled)
            .field("base_url", &self.base_url)
            .field("token", &"[REDACTED]")
            .field("team_id", &self.team_id)
            .field("dm_allowed_users", &self.dm_allowed_users)
            .field("max_attachment_bytes", &self.max_attachment_bytes)
            .finish()
    }
}

#[derive(Debug, Clone, Default)]
pub struct MattermostPermissions {
    pub team_filter: Option<Vec<String>>,
    pub channel_filter: HashMap<String, Vec<String>>,
    pub dm_allowed_users: Vec<String>,
}
```

#### TOML Deserialization Types

```rust
#[derive(Deserialize)]
struct TomlMattermostConfig {
    #[serde(default)]
    enabled: bool,
    base_url: Option<String>,
    token: Option<String>,
    team_id: Option<String>,
    #[serde(default)]
    instances: Vec<TomlMattermostInstanceConfig>,
    #[serde(default)]
    dm_allowed_users: Vec<String>,
    #[serde(default = "default_max_attachment_bytes")]
    max_attachment_bytes: usize,
}

fn default_max_attachment_bytes() -> usize {
    10 * 1024 * 1024 // 10 MB
}

#[derive(Deserialize)]
struct TomlMattermostInstanceConfig {
    name: String,
    #[serde(default)]
    enabled: bool,
    base_url: Option<String>,
    token: Option<String>,
    team_id: Option<String>,
    #[serde(default)]
    dm_allowed_users: Vec<String>,
    #[serde(default = "default_max_attachment_bytes")]
    max_attachment_bytes: usize,
}
```

#### URL Validation at Config Load

```rust
fn validate_mattermost_url(url: &str) -> Result<()> {
    let parsed = url::Url::parse(url)
        .map_err(|e| ConfigError::Invalid(format!("invalid mattermost base_url: {e}")))?;
    
    match parsed.scheme() {
        "https" => Ok(()),
        "http" => {
            tracing::warn!("mattermost base_url uses http instead of https");
            Ok(())
        }
        scheme => Err(ConfigError::Invalid(format!(
            "mattermost base_url must be http or https, got: {scheme}"
        )).into()),
    }
}
```

#### Update is_named_adapter_platform

```rust
fn is_named_adapter_platform(platform: &str) -> bool {
    matches!(
        platform,
        "discord" | "slack" | "telegram" | "twitch" | "email" | "mattermost"
    )
}
```

#### Add to build_adapter_validation_states

```rust
if let Some(mattermost) = &messaging.mattermost {
    let named_instances = validate_instance_names(
        "mattermost",
        mattermost.instances.iter().map(|i| i.name.as_str()),
    )?;
    let default_present = !mattermost.base_url.trim().is_empty() 
        && !mattermost.token.trim().is_empty();
    validate_runtime_keys("mattermost", default_present, &named_instances)?;
    
    if default_present {
        validate_mattermost_url(&mattermost.base_url)?;
    }
    for instance in &mattermost.instances {
        if instance.enabled && !instance.base_url.is_empty() {
            validate_mattermost_url(&instance.base_url)?;
        }
    }
    
    states.insert(
        "mattermost",
        AdapterValidationState {
            default_present,
            named_instances,
        },
    );
}
```

### 2. Config TOML Schema

```toml
[messaging.mattermost]
enabled = true
base_url = "https://mattermost.example.com"
token = "your-bot-personal-access-token"
team_id = "team-id-optional-default"
dm_allowed_users = ["user-id-1", "user-id-2"]
max_attachment_bytes = 10485760  # 10 MB default

[[messaging.mattermost.instances]]
name = "internal"
enabled = true
base_url = "https://internal.company.com"
token = "env:MATTERMOST_INTERNAL_TOKEN"
team_id = "internal-team-id"
max_attachment_bytes = 5242880  # 5 MB for this instance

[[bindings]]
agent_id = "default"
channel = "mattermost"
team_id = "team-id-here"
channel_ids = ["channel-id-1", "channel-id-2"]
dm_allowed_users = ["user-id-1"]
```

### 3. Adapter Implementation (src/messaging/mattermost.rs)

```rust
//! Mattermost messaging adapter using custom HTTP + WebSocket client.

use crate::config::MattermostPermissions;
use crate::messaging::apply_runtime_adapter_to_conversation_id;
use crate::messaging::traits::{HistoryMessage, InboundStream, Messaging};
use crate::{InboundMessage, MessageContent, OutboundResponse, StatusUpdate};
use crate::error::{AdapterError, Result};

use anyhow::Context as _;
use arc_swap::ArcSwap;
use futures_util::{SinkExt, StreamExt};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, OnceCell, RwLock};
use tokio_tungstenite::{
    connect_async,
    tungstenite::{Error as WsError, Message as WsMessage},
    MaybeTlsStream, WebSocketStream,
};

const MAX_MESSAGE_LENGTH: usize = 16_383;
const STREAM_EDIT_THROTTLE: Duration = Duration::from_millis(500);
const TYPING_INDICATOR_INTERVAL: Duration = Duration::from_secs(5);
const WS_RECONNECT_BASE_DELAY: Duration = Duration::from_secs(1);
const WS_RECONNECT_MAX_DELAY: Duration = Duration::from_secs(60);
const HTTP_TIMEOUT: Duration = Duration::from_secs(30);

pub struct MattermostAdapter {
    runtime_key: Arc<str>,
    base_url: url::Url,
    token: Arc<str>,
    default_team_id: Option<Arc<str>>,
    max_attachment_bytes: usize,
    client: Client,
    permissions: Arc<ArcSwap<MattermostPermissions>>,
    bot_user_id: OnceCell<Arc<str>>,
    bot_username: OnceCell<Arc<str>>,
    active_messages: Arc<RwLock<HashMap<String, ActiveStream>>>,
    typing_tasks: Arc<RwLock<HashMap<String, tokio::task::JoinHandle<()>>>>,
    shutdown_tx: Arc<RwLock<Option<mpsc::Sender<()>>>>,
    ws_task: Arc<RwLock<Option<tokio::task::JoinHandle<()>>>>,
}

struct ActiveStream {
    post_id: Arc<str>,
    channel_id: Arc<str>,
    last_edit: Instant,
    accumulated_text: String,
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
    pub fn new(
        runtime_key: impl Into<Arc<str>>,
        base_url: &str,
        token: impl Into<Arc<str>>,
        default_team_id: Option<Arc<str>>,
        max_attachment_bytes: usize,
        permissions: Arc<ArcSwap<MattermostPermissions>>,
    ) -> Result<Self> {
        let base_url = url::Url::parse(base_url)
            .map_err(|e| AdapterError::ConfigInvalid(format!("invalid base_url: {e}")))?;
        
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
            active_messages: Arc::new(RwLock::new(HashMap::new())),
            typing_tasks: Arc::new(RwLock::new(HashMap::new())),
            shutdown_tx: Arc::new(RwLock::new(None)),
            ws_task: Arc::new(RwLock::new(None)),
        })
    }

    fn api_url(&self, path: &str) -> url::Url {
        let mut url = self.base_url.clone();
        url.path_segments_mut()
            .expect("base_url is valid")
            .extend(&["api", "v4"])
            .extend(path.trim_start_matches('/').split('/'));
        url
    }

    fn ws_url(&self) -> url::Url {
        let mut url = self.base_url.clone();
        url.set_scheme(match self.base_url.scheme() {
            "https" => "wss",
            "http" => "ws",
            other => other,
        }).expect("scheme is valid");
        url.path_segments_mut()
            .expect("base_url is valid")
            .extend(&["api", "v4", "websocket"]);
        url
    }

    fn extract_channel_id<'a>(&self, message: &'a InboundMessage) -> Result<&'a str> {
        message
            .metadata
            .get("mattermost_channel_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| AdapterError::MissingMetadata("mattermost_channel_id".into()).into())
    }

    fn extract_post_id<'a>(&self, message: &'a InboundMessage) -> Result<&'a str> {
        message
            .metadata
            .get("mattermost_post_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| AdapterError::MissingMetadata("mattermost_post_id".into()).into())
    }

    fn validate_id(id: &str) -> Result<()> {
        if id.is_empty() || id.len() > 64 {
            return Err(AdapterError::InvalidId(id.into()).into());
        }
        if !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
            return Err(AdapterError::InvalidId(id.into()).into());
        }
        Ok(())
    }

    async fn stop_typing(&self, channel_id: &str) {
        if let Some(handle) = self.typing_tasks.write().await.remove(channel_id) {
            handle.abort();
        }
    }

    async fn start_typing(&self, channel_id: &str) {
        // Typing indicators expire after ~5 seconds. Spawn a task that re-sends on
        // TYPING_INDICATOR_INTERVAL so the indicator remains visible during long operations.
        // The task is stored in typing_tasks so stop_typing can abort it.
        let Some(user_id) = self.bot_user_id.get().cloned() else { return };
        let channel_id = channel_id.to_string();
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
                        "channel_id": channel_id,
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

    async fn create_post(
        &self,
        channel_id: &str,
        message: &str,
        root_id: Option<&str>,
    ) -> Result<MattermostPost> {
        Self::validate_id(channel_id)?;
        if let Some(rid) = root_id {
            Self::validate_id(rid)?;
        }

        let response = self.client
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
            return Err(AdapterError::ApiError {
                endpoint: "/posts".into(),
                status: status.as_u16(),
                message: body,
            }.into());
        }

        response
            .json()
            .await
            .context("failed to parse post response")
            .map_err(Into::into)
    }

    async fn edit_post(&self, post_id: &str, message: &str) -> Result<()> {
        Self::validate_id(post_id)?;

        let response = self.client
            .put(self.api_url(&format!("/posts/{post_id}")))
            .bearer_auth(self.token.as_ref())
            .json(&serde_json::json!({ "message": message }))
            .send()
            .await
            .context("failed to edit post")?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(AdapterError::ApiError {
                endpoint: format!("/posts/{post_id}"),
                status: status.as_u16(),
                message: body,
            }.into());
        }

        Ok(())
    }

    async fn get_channel_posts(
        &self,
        channel_id: &str,
        before_post_id: Option<&str>,
        limit: u32,
    ) -> Result<MattermostPostList> {
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

        let response = self.client
            .get(url)
            .bearer_auth(self.token.as_ref())
            .send()
            .await
            .context("failed to fetch channel posts")?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(AdapterError::ApiError {
                endpoint: format!("/channels/{channel_id}/posts"),
                status: status.as_u16(),
                message: body,
            }.into());
        }

        response
            .json()
            .await
            .context("failed to parse posts response")
            .map_err(Into::into)
    }

    fn build_inbound_message(
        &self,
        post: MattermostPost,
        team_id: Option<String>,
    ) -> Option<InboundMessage> {
        let bot_id = self.bot_user_id.get()?;
        if &post.user_id == bot_id.as_ref() {
            return None;
        }

        let permissions = self.permissions.load();
        
        if let Some(team_filter) = &permissions.team_filter {
            if let Some(ref tid) = team_id {
                if !team_filter.contains(tid) {
                    return None;
                }
            }
        }

        if let Some(channel_filter) = permissions.channel_filter.get(&post.channel_id) {
            if !channel_filter.contains(&post.channel_id) {
                return None;
            }
        }

        // DM channel type is carried in the WebSocket event's `channel_type` field.
        // All Mattermost channel IDs start with lowercase alphanumeric, so channel_id prefix
        // is NOT a reliable DM indicator. Use the channel_type from the event data instead,
        // passed in as an optional parameter. "D" = direct message, "G" = group message.
        let conversation_id = if post.channel_type.as_deref() == Some("D") {
            apply_runtime_adapter_to_conversation_id(
                self.runtime_key.as_ref(),
                format!("mattermost:{}:dm:{}", team_id.as_deref().unwrap_or(""), post.user_id),
            )
        } else {
            apply_runtime_adapter_to_conversation_id(
                self.runtime_key.as_ref(),
                format!("mattermost:{}:{}", team_id.as_deref().unwrap_or(""), post.channel_id),
            )
        };

        let mut metadata = HashMap::new();
        metadata.insert("mattermost_post_id".into(), serde_json::json!(post.id));
        metadata.insert("mattermost_channel_id".into(), serde_json::json!(post.channel_id));
        if let Some(tid) = team_id {
            metadata.insert("mattermost_team_id".into(), serde_json::json!(tid));
        }
        if !post.root_id.is_empty() {
            metadata.insert("mattermost_root_id".into(), serde_json::json!(post.root_id));
        }
        metadata.insert(
            "mattermost_create_at".into(),
            serde_json::json!(post.create_at),
        );

        Some(InboundMessage {
            id: post.id.clone(),
            source: "mattermost".into(),
            adapter: Some(self.runtime_key.to_string()),
            conversation_id,
            sender_id: post.user_id.clone(),
            agent_id: None,
            content: MessageContent::Text(post.message),
            timestamp: chrono::DateTime::from_timestamp_millis(post.create_at)
                .unwrap_or_else(chrono::Utc::now),
            metadata,
            formatted_author: None,
        })
    }
}

impl Messaging for MattermostAdapter {
    fn name(&self) -> &str {
        &self.runtime_key
    }

    async fn start(&self) -> Result<InboundStream> {
        let (inbound_tx, inbound_rx) = mpsc::channel(256);
        let (shutdown_tx, mut shutdown_rx) = mpsc::channel::<()>(1);
        *self.shutdown_tx.write().await = Some(shutdown_tx);

        let me: MattermostUser = self.client
            .get(self.api_url("/users/me"))
            .bearer_auth(self.token.as_ref())
            .send()
            .await
            .context("failed to get bot user")?
            .json()
            .await
            .context("failed to parse user response")?;

        let user_id: Arc<str> = me.id.clone().into();
        let username: Arc<str> = me.username.clone().into();
        
        self.bot_user_id.set(user_id.clone()).expect("set once");
        self.bot_username.set(username.clone()).expect("set once");
        
        tracing::info!(
            adapter = %self.runtime_key,
            bot_id = %user_id,
            bot_username = %username,
            "mattermost connected"
        );

        let ws_url = self.ws_url();
        let runtime_key = self.runtime_key.clone();
        let token = self.token.clone();
        let permissions = self.permissions.clone();
        let bot_user_id = user_id;
        let inbound_tx_clone = inbound_tx.clone();

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
                        
                        if let Ok(msg) = serde_json::to_string(&auth_msg) {
                            if write.send(WsMessage::Text(msg)).await.is_err() {
                                tracing::error!("failed to send websocket auth");
                                continue;
                            }
                        }

                        loop {
                            tokio::select! {
                                _ = shutdown_rx.recv() => {
                                    tracing::info!(adapter = %runtime_key, "mattermost websocket shutting down");
                                    let _ = write.send(WsMessage::Close(None)).await;
                                    return;
                                }
                                
                                msg = read.next() => {
                                    match msg {
                                        Some(Ok(WsMessage::Text(text))) => {
                                            if let Ok(event) = serde_json::from_str::<MattermostWsEvent>(&text) {
                                                if event.event == "posted" {
                                                    if let Some(post_data) = event.data.get("post") {
                                                        if let Ok(post) = serde_json::from_value::<MattermostPost>(post_data.clone()) {
                                                            if post.user_id.as_str() != bot_user_id.as_ref() {
                                                                let team_id = event
                                                                    .broadcast
                                                                    .team_id
                                                                    .clone();
                                                                
                                                                // Load permissions each message so hot-reload takes effect.
                                                                let perms = permissions.load();
                                                                if let Some(msg) = build_message_from_post(
                                                                    &post,
                                                                    &runtime_key,
                                                                    &bot_user_id,
                                                                    &team_id,
                                                                    &perms,
                                                                ) {
                                                                    if inbound_tx_clone.send(msg).await.is_err() {
                                                                        tracing::debug!("inbound channel closed");
                                                                        return;
                                                                    }
                                                                }
                                                            }
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                        Some(Ok(WsMessage::Ping(data))) => {
                                            if write.send(WsMessage::Pong(data)).await.is_err() {
                                                tracing::warn!("failed to send pong");
                                                break;
                                            }
                                        }
                                        Some(Ok(WsMessage::Pong(_))) => {}
                                        Some(Ok(WsMessage::Close(_))) => {
                                            tracing::info!(adapter = %runtime_key, "websocket closed by server");
                                            break;
                                        }
                                        Some(Err(e)) => {
                                            tracing::error!(adapter = %runtime_key, error = %e, "websocket error");
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
                    Err(e) => {
                        tracing::error!(
                            adapter = %runtime_key,
                            error = %e,
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
    ) -> Result<()> {
        let channel_id = self.extract_channel_id(message)?;
        
        match response {
            OutboundResponse::Text(text) => {
                self.stop_typing(channel_id).await;
                // mattermost_root_id: set when the triggering message was itself inside a thread.
                // REPLY_TO_MESSAGE_ID: set by channel.rs on retrigger messages when a branch or
                // worker completes — this is the post ID the agent should thread its reply under.
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
                // Key by message.id, not channel_id. Keying by channel_id would cause
                // concurrent conversations in the same channel to overwrite each other.
                let root_id = message.metadata.get("mattermost_root_id").and_then(|v| v.as_str());
                self.start_typing(channel_id).await;
                let post = self.create_post(channel_id, "\u{200B}", root_id).await?;
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
                let mut active_messages = self.active_messages.write().await;
                if let Some(active) = active_messages.get_mut(&message.id) {
                    active.accumulated_text.push_str(&chunk);

                    if active.last_edit.elapsed() > STREAM_EDIT_THROTTLE {
                        let display_text = if active.accumulated_text.len() > MAX_MESSAGE_LENGTH {
                            let end = active.accumulated_text.floor_char_boundary(MAX_MESSAGE_LENGTH - 3);
                            format!("{}...", &active.accumulated_text[..end])
                        } else {
                            active.accumulated_text.clone()
                        };

                        if let Err(error) = self.edit_post(&active.post_id, &display_text).await {
                            tracing::warn!(%error, "failed to edit streaming message");
                        }
                        active.last_edit = Instant::now();
                    }
                }
            }

            OutboundResponse::StreamEnd => {
                self.stop_typing(channel_id).await;
                // Always flush the final text, even if the last chunk was within the throttle window.
                if let Some(active) = self.active_messages.write().await.remove(&message.id) {
                    if let Err(error) = self.edit_post(&active.post_id, &active.accumulated_text).await {
                        tracing::warn!(%error, "failed to finalize streaming message");
                    }
                }
            }

            OutboundResponse::Status(status) => self.send_status(message, status).await?,
            
            OutboundResponse::Reaction(emoji) => {
                let post_id = self.extract_post_id(message)?;
                let emoji_name = emoji.trim_matches(':');
                
                let response = self.client
                    .post(self.api_url("/reactions"))
                    .bearer_auth(self.token.as_ref())
                    .json(&serde_json::json!({
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
            
            OutboundResponse::File { filename, data, mime_type, caption } => {
                if data.len() > self.max_attachment_bytes {
                    return Err(AdapterError::FileTooLarge {
                        size: data.len(),
                        max: self.max_attachment_bytes,
                    }.into());
                }

                let part = reqwest::multipart::Part::bytes(data)
                    .file_name(filename.clone())
                    .mime_str(&mime_type)
                    .context("invalid mime type")?;
                
                let form = reqwest::multipart::Form::new()
                    .part("files", part)
                    .text("channel_id", channel_id.to_string());

                let response = self.client
                    .post(self.api_url("/files"))
                    .bearer_auth(self.token.as_ref())
                    .multipart(form)
                    .send()
                    .await
                    .context("failed to upload file")?;

                if !response.status().is_success() {
                    let body = response.text().await.unwrap_or_default();
                    return Err(AdapterError::ApiError {
                        endpoint: "/files".into(),
                        status: response.status().as_u16(),
                        message: body,
                    }.into());
                }

                let upload: MattermostFileUpload = response
                    .json()
                    .await
                    .context("failed to parse file upload response")?;

                let file_ids: Vec<_> = upload.file_infos.iter().map(|f| f.id.as_str()).collect();
                self.client
                    .post(self.api_url("/posts"))
                    .bearer_auth(self.token.as_ref())
                    .json(&serde_json::json!({
                        "channel_id": channel_id,
                        "message": caption.unwrap_or_default(),
                        "file_ids": file_ids,
                    }))
                    .send()
                    .await
                    .context("failed to create post with file")?;
            }
            
            _ => {
                tracing::debug!(?response, "mattermost adapter does not support this response type");
            }
        }

        Ok(())
    }

    async fn send_status(
        &self,
        message: &InboundMessage,
        status: StatusUpdate,
    ) -> Result<()> {
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
    ) -> Result<Vec<HistoryMessage>> {
        let channel_id = self.extract_channel_id(message)?;
        let before_post_id = message.metadata.get("mattermost_post_id").and_then(|v| v.as_str());
        
        let capped_limit = limit.min(200) as u32;
        let posts = self.get_channel_posts(channel_id, before_post_id, capped_limit).await?;
        
        let bot_id = self.bot_user_id.get().map(|s| s.as_ref().to_string());

        // Sort by create_at before mapping so chronological order is preserved.
        // Mapping to HistoryMessage discards the timestamp field.
        let mut posts_vec: Vec<_> = posts
            .posts
            .into_values()
            .filter(|p| bot_id.as_deref() != Some(p.user_id.as_str()))
            .collect();
        posts_vec.sort_by_key(|p| p.create_at);

        let history: Vec<HistoryMessage> = posts_vec
            .into_iter()
            .map(|p| HistoryMessage {
                author: p.user_id,
                content: p.message,
                is_bot: false,
            })
            .collect();

        Ok(history)
    }

    async fn health_check(&self) -> Result<()> {
        let response = self.client
            .get(self.api_url("/system/ping"))
            .bearer_auth(self.token.as_ref())
            .send()
            .await
            .context("health check request failed")?;

        if !response.status().is_success() {
            return Err(AdapterError::HealthCheckFailed(
                format!("status: {}", response.status())
            ).into());
        }

        Ok(())
    }

    async fn shutdown(&self) -> Result<()> {
        if let Some(tx) = self.shutdown_tx.write().await.take() {
            let _ = tx.send(()).await;
        }
        
        if let Some(handle) = self.ws_task.write().await.take() {
            handle.abort();
        }
        
        self.typing_tasks.write().await.clear();
        self.active_messages.write().await.clear();
        
        tracing::info!(adapter = %self.runtime_key, "mattermost adapter shut down");
        Ok(())
    }

    async fn broadcast(&self, target: &str, response: OutboundResponse) -> crate::Result<()> {
        // target is a channel_id extracted by resolve_broadcast_target().
        match response {
            OutboundResponse::Text(text) => {
                for chunk in split_message(&text, MAX_MESSAGE_LENGTH) {
                    self.create_post(target, &chunk, None).await?;
                }
            }
            OutboundResponse::File { filename, data, mime_type, caption } => {
                if data.len() > self.max_attachment_bytes {
                    return Err(AdapterError::FileTooLarge {
                        size: data.len(),
                        max: self.max_attachment_bytes,
                    }.into());
                }
                let part = reqwest::multipart::Part::bytes(data)
                    .file_name(filename)
                    .mime_str(&mime_type)
                    .context("invalid mime type")?;
                let form = reqwest::multipart::Form::new()
                    .part("files", part)
                    .text("channel_id", target.to_string());
                let upload: MattermostFileUpload = self.client
                    .post(self.api_url("/files"))
                    .bearer_auth(self.token.as_ref())
                    .multipart(form)
                    .send()
                    .await
                    .context("failed to upload file")?
                    .json()
                    .await
                    .context("failed to parse file upload response")?;
                let file_ids: Vec<_> = upload.file_infos.iter().map(|f| f.id.as_str()).collect();
                self.client
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
            }
            other => {
                tracing::debug!(?other, "mattermost broadcast does not support this response type");
            }
        }
        Ok(())
    }
}

fn build_message_from_post(
    post: &MattermostPost,
    runtime_key: &str,
    bot_user_id: &str,
    team_id: &Option<String>,
    permissions: &MattermostPermissions,
) -> Option<InboundMessage> {
    if post.user_id == bot_user_id {
        return None;
    }

    if let Some(team_filter) = &permissions.team_filter {
        if let Some(ref tid) = team_id {
            if !team_filter.contains(tid) {
                return None;
            }
        }
    }

    let conversation_id = apply_runtime_adapter_to_conversation_id(
        runtime_key,
        format!("mattermost:{}:{}", team_id.as_deref().unwrap_or(""), post.channel_id),
    );

    let mut metadata = HashMap::new();

    // Standard keys — required by channel.rs and channel_history.rs.
    // metadata_keys::MESSAGE_ID is read by extract_message_id(), which feeds
    // branch/worker reply threading via REPLY_TO_MESSAGE_ID in retrigger metadata.
    metadata.insert(
        crate::metadata_keys::MESSAGE_ID.into(),
        serde_json::json!(&post.id),
    );

    // Platform-specific keys used within the adapter (respond, fetch_history, etc.).
    metadata.insert("mattermost_post_id".into(), serde_json::json!(&post.id));
    metadata.insert("mattermost_channel_id".into(), serde_json::json!(&post.channel_id));
    if let Some(tid) = team_id {
        metadata.insert("mattermost_team_id".into(), serde_json::json!(tid));
    }
    if !post.root_id.is_empty() {
        metadata.insert("mattermost_root_id".into(), serde_json::json!(&post.root_id));
    }

    Some(InboundMessage {
        id: post.id.clone(),
        source: "mattermost".into(),
        adapter: Some(runtime_key.to_string()),
        conversation_id,
        sender_id: post.user_id.clone(),
        agent_id: None,
        content: MessageContent::Text(post.message.clone()),
        timestamp: chrono::DateTime::from_timestamp_millis(post.create_at)
            .unwrap_or_else(chrono::Utc::now),
        metadata,
        formatted_author: None,
    })
}

// --- API Types ---

#[derive(Debug, Clone, Deserialize)]
struct MattermostUser {
    id: String,
    username: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct MattermostPost {
    id: String,
    create_at: i64,
    update_at: i64,
    user_id: String,
    channel_id: String,
    root_id: String,
    message: String,
    // "D" = direct message, "G" = group DM, "O" = public, "P" = private.
    // Present in WebSocket events; absent in REST list responses.
    #[serde(default)]
    channel_type: Option<String>,
    #[serde(default)]
    file_ids: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct MattermostPostList {
    #[serde(default)]
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
    channel_id: Option<String>,
    #[serde(default)]
    team_id: Option<String>,
    #[serde(default)]
    user_id: Option<String>,
}

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
        
        let search_region = &remaining[..max_len];
        let break_point = search_region
            .rfind('\n')
            .or_else(|| search_region.rfind(' '))
            .unwrap_or(max_len);
        
        let end = remaining.floor_char_boundary(break_point);
        chunks.push(remaining[..end].to_string());
        remaining = remaining[end..].trim_start_matches('\n').trim_start();
    }
    
    chunks
}

// --- Error Types (add to src/error.rs) ---

#[derive(Debug, thiserror::Error)]
pub enum AdapterError {
    #[error("missing metadata field: {0}")]
    MissingMetadata(String),
    
    #[error("invalid id format: {0}")]
    InvalidId(String),
    
    #[error("API error at {endpoint}: {status} - {message}")]
    ApiError {
        endpoint: String,
        status: u16,
        message: String,
    },
    
    #[error("file too large: {size} bytes (max: {max})")]
    FileTooLarge { size: usize, max: usize },
    
    #[error("health check failed: {0}")]
    HealthCheckFailed(String),
    
    #[error("invalid configuration: {0}")]
    ConfigInvalid(String),
    
    #[error("adapter not initialized")]
    NotInitialized,
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn test_validate_id_valid() {
        assert!(MattermostAdapter::validate_id("abc123").is_ok());
        assert!(MattermostAdapter::validate_id("a-b_c").is_ok());
    }

    #[test]
    fn test_validate_id_invalid() {
        assert!(MattermostAdapter::validate_id("").is_err());
        assert!(MattermostAdapter::validate_id("has spaces").is_err());
        assert!(MattermostAdapter::validate_id("has/slash").is_err());
    }
}
```

### 4. Module Registration (src/messaging/mod.rs)

```rust
pub mod mattermost;

pub use mattermost::MattermostAdapter;
```

### 5. Target Resolution (src/messaging/target.rs)

Add to `resolve_broadcast_target()`:

```rust
"mattermost" => {
    if let Some(channel_id) = channel
        .platform_meta
        .as_ref()
        .and_then(|meta| meta.get("mattermost_channel_id"))
        .and_then(json_value_to_string)
    {
        channel_id
    } else {
        let parts: Vec<&str> = channel.id.split(':').collect();
        match parts.as_slice() {
            ["mattermost", _, channel_id] => (*channel_id).to_string(),
            ["mattermost", _, "dm", user_id] => format!("dm:{user_id}"),
            ["mattermost", _, _, channel_id] => (*channel_id).to_string(),
            _ => return None,
        }
    }
}
```

Add to `normalize_target()`:

```rust
"mattermost" => normalize_mattermost_target(trimmed),
```

Add normalization function:

```rust
fn normalize_mattermost_target(raw_target: &str) -> Option<String> {
    let target = strip_repeated_prefix(raw_target, "mattermost");
    
    if let Some((_, channel_id)) = target.split_once(':') {
        if !channel_id.is_empty() {
            return Some(channel_id.to_string());
        }
        return None;
    }
    
    if let Some(user_id) = target.strip_prefix("dm:") {
        if !user_id.is_empty() {
            return Some(format!("dm:{user_id}"));
        }
        return None;
    }
    
    if target.is_empty() {
        None
    } else {
        Some(target.to_string())
    }
}
```

### 6. Binding Support (src/config.rs)

The `Binding` struct currently has `guild_id`, `workspace_id`, and `chat_id` but **no `team_id`
field**. Add it:

```rust
pub struct Binding {
    // ... existing fields ...
    pub team_id: Option<String>,  // NEW — Mattermost team ID filter
}
```

Add to the TOML deserialization type (`TomlBinding` or equivalent):

```rust
#[serde(default)]
team_id: Option<String>,
```

Add to `Binding::matches()` after the existing channel-type checks:

```rust
// Mattermost team filter
if let Some(team_id) = &self.team_id {
    if self.channel == "mattermost" {
        let message_team = message
            .metadata
            .get("mattermost_team_id")
            .and_then(|v| v.as_str());
        if message_team != Some(team_id.as_str()) {
            return false;
        }
    }
}

// Mattermost channel filter
if !self.channel_ids.is_empty() && self.channel == "mattermost" {
    let message_channel = message
        .metadata
        .get("mattermost_channel_id")
        .and_then(|v| v.as_str());
    if !self.channel_ids.iter().any(|id| Some(id.as_str()) == message_channel) {
        return false;
    }
}
```

### 7. Main.rs Registration

Add following the existing pattern:

```rust
// Shared Mattermost permissions (hot-reloadable via file watcher)
*mattermost_permissions = config.messaging.mattermost.as_ref().map(|mm_config| {
    let perms = MattermostPermissions::from_config(mm_config, &config.bindings);
    Arc::new(ArcSwap::from_pointee(perms))
});
if let Some(perms) = &*mattermost_permissions {
    api_state.set_mattermost_permissions(perms.clone()).await;
}

if let Some(mm_config) = &config.messaging.mattermost
    && mm_config.enabled
{
    if !mm_config.base_url.is_empty() && !mm_config.token.is_empty() {
        match MattermostAdapter::new(
            Arc::from("mattermost"),
            &mm_config.base_url,
            Arc::from(mm_config.token.clone()),
            mm_config.team_id.clone().map(Arc::from),
            mm_config.max_attachment_bytes,
            mattermost_permissions.clone().ok_or_else(|| {
                anyhow::anyhow!("mattermost permissions not initialized")
            })?,
        ) {
            Ok(adapter) => {
                new_messaging_manager.register(adapter).await;
            }
            Err(error) => {
                tracing::error!(%error, "failed to build mattermost adapter");
            }
        }
    }

    for instance in mm_config
        .instances
        .iter()
        .filter(|i| i.enabled)
    {
        if instance.base_url.is_empty() || instance.token.is_empty() {
            tracing::warn!(adapter = %instance.name, "skipping enabled mattermost instance with missing config");
            continue;
        }
        let runtime_key = binding_runtime_adapter_key("mattermost", Some(instance.name.as_str()));
        let perms = Arc::new(ArcSwap::from_pointee(
            MattermostPermissions::from_instance_config(instance, &config.bindings),
        ));
        match MattermostAdapter::new(
            Arc::from(runtime_key),
            &instance.base_url,
            Arc::from(instance.token.clone()),
            instance.team_id.clone().map(Arc::from),
            instance.max_attachment_bytes,
            perms,
        ) {
            Ok(adapter) => {
                new_messaging_manager.register(adapter).await;
            }
            Err(error) => {
                tracing::error!(adapter = %instance.name, %error, "failed to build named mattermost adapter");
            }
        }
    }
}
```

### 8. Permissions Implementation

```rust
impl MattermostPermissions {
    pub fn from_config(config: &MattermostConfig, bindings: &[Binding]) -> Self {
        Self::from_bindings_for_adapter(
            config.dm_allowed_users.clone(),
            bindings,
            None,
        )
    }

    pub fn from_instance_config(
        instance: &MattermostInstanceConfig,
        bindings: &[Binding],
    ) -> Self {
        Self::from_bindings_for_adapter(
            instance.dm_allowed_users.clone(),
            bindings,
            Some(instance.name.as_str()),
        )
    }

    fn from_bindings_for_adapter(
        seed_dm_allowed_users: Vec<String>,
        bindings: &[Binding],
        adapter_selector: Option<&str>,
    ) -> Self {
        let mm_bindings: Vec<&Binding> = bindings
            .iter()
            .filter(|b| {
                b.channel == "mattermost"
                    && binding_adapter_selector_matches(b, adapter_selector)
            })
            .collect();

        let team_filter = {
            let team_ids: Vec<String> = mm_bindings
                .iter()
                .filter_map(|b| b.team_id.clone())
                .collect();
            if team_ids.is_empty() { None } else { Some(team_ids) }
        };

        let channel_filter = {
            let mut filter: HashMap<String, Vec<String>> = HashMap::new();
            for binding in &mm_bindings {
                if let Some(team_id) = &binding.team_id
                    && !binding.channel_ids.is_empty()
                {
                    filter
                        .entry(team_id.clone())
                        .or_default()
                        .extend(binding.channel_ids.clone());
                }
            }
            filter
        };

        let mut dm_allowed_users = seed_dm_allowed_users;
        for binding in &mm_bindings {
            for id in &binding.dm_allowed_users {
                if !dm_allowed_users.contains(id) {
                    dm_allowed_users.push(id.clone());
                }
            }
        }

        Self {
            team_filter,
            channel_filter,
            dm_allowed_users,
        }
    }
}
```

### 9. Error Types (src/error.rs)

Add to the existing Error enum:

```rust
#[derive(Debug, thiserror::Error)]
pub enum Error {
    // ... existing variants
    
    #[error("adapter error: {0}")]
    Adapter(#[from] crate::messaging::mattermost::AdapterError),
}
```

---

## API Endpoints Used

| Endpoint | Method | Purpose |
|----------|--------|---------|
| `/users/me` | GET | Bot identity |
| `/users/{id}/typing` | POST | Typing indicator |
| `/posts` | POST | Create message |
| `/posts/{id}` | PUT | Edit message (streaming) |
| `/posts/{id}` | DELETE | Delete message |
| `/channels/{id}/posts` | GET | History fetch |
| `/files` | POST | File upload |
| `/reactions` | POST | Add reaction |
| `/system/ping` | GET | Health check |
| WebSocket `/api/v4/websocket` | - | Real-time events |

---

## Dependencies

The project already has `tokio-tungstenite = "0.28"` in `Cargo.toml`. Do **not** add a `0.24`
entry — it conflicts with the existing version. Verify the existing entry includes `native-tls`:

```toml
# Ensure this is already present with the native-tls feature; do not downgrade:
tokio-tungstenite = { version = "0.28", features = ["native-tls"] }

# Add if not already present:
url = "2.5"
```

`futures-util` is already an indirect dependency; verify before adding explicitly.

---

## Feature Support Matrix

| Feature | Priority | Status |
|---------|----------|--------|
| Text messages | Required | Planned |
| Streaming edits | Required | Planned |
| Typing indicator | Required | Planned |
| Thread replies | High | Planned |
| File attachments | Medium | Planned |
| Reactions | Medium | Planned |
| History fetch | Medium | Planned |
| Rich formatting (markdown) | Low | Planned |
| Interactive buttons | Low | Future |
| Slash commands | Low | Future |

---

## Testing Strategy

1. **Unit tests**: Message splitting, ID validation, target normalization, permission filtering
2. **Integration tests**: Mock Mattermost API server (mockito)
3. **Manual testing**: Against real Mattermost instance

---

## Migration Notes

- No database migrations required
- Configuration is additive only
- No breaking changes to existing adapters

---

## Checklist for Implementation

- [ ] Add `MattermostConfig` and related types to `src/config.rs`
- [ ] Add TOML deserialization types
- [ ] Add URL validation
- [ ] Update `is_named_adapter_platform`
- [ ] Add `AdapterError` to `src/error.rs`
- [ ] Create `src/messaging/mattermost.rs` with full implementation
- [ ] Add to `src/messaging/mod.rs`
- [ ] Update `src/messaging/target.rs` for broadcast resolution
- [ ] Add main.rs registration
- [ ] Add unit tests
- [ ] Add integration tests with mock server
- [ ] Update README.md with Mattermost configuration docs
