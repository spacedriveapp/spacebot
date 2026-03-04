//! Types for communicating with OpenCode's HTTP API.
//!
//! Only the subset of the API surface that Spacebot needs is modeled here.
//! OpenCode has a much larger API (PTY, LSP, TUI, MCP, etc.) that we ignore.
//!
//! Every SSE event from OpenCode follows the envelope: `{ type: "...", properties: { ... } }`.
//! The `properties` content varies per event type.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// -- Request types --

/// Body for `POST /session` (create session).
#[derive(Debug, Serialize)]
pub struct CreateSessionRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

/// A single part within a message prompt.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PartInput {
    Text {
        text: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        synthetic: Option<bool>,
    },
    File {
        mime: String,
        url: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        filename: Option<String>,
    },
}

/// Model selection for a prompt.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelParam {
    pub provider_id: String,
    pub model_id: String,
}

/// Body for `POST /session/{id}/message` (send prompt).
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SendPromptRequest {
    pub parts: Vec<PartInput>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<ModelParam>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
}

/// Body for `POST /permission/{id}/reply`.
#[derive(Debug, Serialize)]
pub struct PermissionReplyRequest {
    pub reply: PermissionReply,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// Permission reply options.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionReply {
    Once,
    Always,
    Reject,
}

/// A single question answer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuestionAnswer {
    pub label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// Body for `POST /question/{id}/reply`.
#[derive(Debug, Serialize)]
pub struct QuestionReplyRequest {
    pub answers: Vec<QuestionAnswer>,
}

// -- Response types --

/// Session object returned by the API.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Session {
    pub id: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub parent_id: Option<String>,
}

/// Health check response from `GET /global/health` or `GET /api/health`.
#[derive(Debug, Deserialize)]
pub struct HealthResponse {
    #[serde(default)]
    pub healthy: bool,
    #[serde(default)]
    pub version: Option<String>,
}

/// Time span for message/part timing.
#[derive(Debug, Clone, Deserialize)]
pub struct TimeSpan {
    #[serde(default)]
    pub start: Option<f64>,
    #[serde(default)]
    pub end: Option<f64>,
}

/// A message in a session.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageInfo {
    pub id: String,
    pub role: String,
    #[serde(rename = "sessionID", default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub time: Option<TimeSpan>,
}

// -- SSE Event types --
//
// Every SSE event is `{ type: "event.name", properties: { ... } }`.
// We use a two-level deserialization: first extract the envelope, then
// match on `type` and parse `properties` accordingly.

/// Raw SSE event envelope from OpenCode.
#[derive(Debug, Clone, Deserialize)]
pub struct SseEventEnvelope {
    #[serde(rename = "type")]
    pub event_type: String,
    #[serde(default)]
    pub properties: serde_json::Value,
}

/// Parsed SSE event. Constructed from `SseEventEnvelope` after matching on type.
#[derive(Debug, Clone)]
pub enum SseEvent {
    MessageUpdated {
        info: Option<MessageInfo>,
    },
    MessagePartUpdated {
        part: Part,
        delta: Option<String>,
    },
    SessionIdle {
        session_id: String,
    },
    SessionError {
        session_id: Option<String>,
        error: Option<serde_json::Value>,
    },
    SessionStatus {
        session_id: String,
        status: SessionStatusPayload,
    },
    PermissionAsked(PermissionRequest),
    PermissionReplied {
        session_id: String,
        request_id: String,
        reply: String,
    },
    QuestionAsked(QuestionRequest),
    QuestionReplied {
        session_id: String,
        request_id: String,
    },
    Unknown(String),
}

impl SseEvent {
    /// Parse from an envelope. Returns `Unknown` for unrecognized event types.
    pub fn from_envelope(envelope: SseEventEnvelope) -> Self {
        let props = envelope.properties;

        match envelope.event_type.as_str() {
            "message.updated" => {
                let info = serde_json::from_value::<MessageUpdatedProps>(props)
                    .ok()
                    .and_then(|p| p.info);
                SseEvent::MessageUpdated { info }
            }
            "message.part.updated" => {
                match serde_json::from_value::<MessagePartUpdatedProps>(props) {
                    Ok(p) => SseEvent::MessagePartUpdated {
                        part: p.part,
                        delta: p.delta,
                    },
                    Err(error) => {
                        tracing::trace!(%error, "failed to parse message.part.updated properties");
                        SseEvent::Unknown("message.part.updated (parse error)".into())
                    }
                }
            }
            "session.idle" => match serde_json::from_value::<SessionIdProps>(props) {
                Ok(p) => SseEvent::SessionIdle {
                    session_id: p.session_id,
                },
                Err(_) => SseEvent::Unknown("session.idle (parse error)".into()),
            },
            "session.error" => {
                let p = serde_json::from_value::<SessionErrorProps>(props).unwrap_or_default();
                SseEvent::SessionError {
                    session_id: p.session_id,
                    error: p.error,
                }
            }
            "session.status" => match serde_json::from_value::<SessionStatusProps>(props) {
                Ok(p) => SseEvent::SessionStatus {
                    session_id: p.session_id,
                    status: p.status,
                },
                Err(_) => SseEvent::Unknown("session.status (parse error)".into()),
            },
            "permission.asked" => match serde_json::from_value::<PermissionRequest>(props) {
                Ok(p) => SseEvent::PermissionAsked(p),
                Err(_) => SseEvent::Unknown("permission.asked (parse error)".into()),
            },
            "permission.replied" => match serde_json::from_value::<PermissionRepliedProps>(props) {
                Ok(p) => SseEvent::PermissionReplied {
                    session_id: p.session_id,
                    request_id: p.request_id,
                    reply: p.reply,
                },
                Err(_) => SseEvent::Unknown("permission.replied (parse error)".into()),
            },
            "question.asked" => match serde_json::from_value::<QuestionRequest>(props) {
                Ok(p) => SseEvent::QuestionAsked(p),
                Err(_) => SseEvent::Unknown("question.asked (parse error)".into()),
            },
            "question.replied" => match serde_json::from_value::<QuestionRepliedProps>(props) {
                Ok(p) => SseEvent::QuestionReplied {
                    session_id: p.session_id,
                    request_id: p.request_id,
                },
                Err(_) => SseEvent::Unknown("question.replied (parse error)".into()),
            },
            other => SseEvent::Unknown(other.to_string()),
        }
    }
}

// -- Properties structs for each event type --

#[derive(Debug, Deserialize)]
struct MessageUpdatedProps {
    #[serde(default)]
    info: Option<MessageInfo>,
}

#[derive(Debug, Deserialize)]
struct MessagePartUpdatedProps {
    part: Part,
    #[serde(default)]
    delta: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SessionIdProps {
    #[serde(rename = "sessionID")]
    session_id: String,
}

#[derive(Debug, Default, Deserialize)]
struct SessionErrorProps {
    #[serde(rename = "sessionID", default)]
    session_id: Option<String>,
    #[serde(default)]
    error: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct SessionStatusProps {
    #[serde(rename = "sessionID")]
    session_id: String,
    status: SessionStatusPayload,
}

#[derive(Debug, Deserialize)]
struct PermissionRepliedProps {
    #[serde(rename = "sessionID")]
    session_id: String,
    #[serde(rename = "requestID")]
    request_id: String,
    reply: String,
}

#[derive(Debug, Deserialize)]
struct QuestionRepliedProps {
    #[serde(rename = "sessionID")]
    session_id: String,
    #[serde(rename = "requestID")]
    request_id: String,
}

// -- Part types --

/// A content part within a message. Discriminated by `type` field.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type")]
pub enum Part {
    #[serde(rename = "text")]
    Text {
        id: String,
        #[serde(rename = "sessionID", default)]
        session_id: Option<String>,
        #[serde(rename = "messageID", default)]
        message_id: Option<String>,
        #[serde(default)]
        text: String,
        #[serde(default)]
        time: Option<TimeSpan>,
    },
    #[serde(rename = "tool")]
    Tool {
        id: String,
        #[serde(rename = "sessionID", default)]
        session_id: Option<String>,
        #[serde(rename = "messageID", default)]
        message_id: Option<String>,
        #[serde(rename = "callID", default)]
        call_id: Option<String>,
        /// The tool name (e.g. "bash", "read", "edit", "task").
        #[serde(default)]
        tool: Option<String>,
        /// Tool execution state. This is a tagged object with `status` as discriminant,
        /// not a simple enum. Contains `input`, `output`, `title`, `time`, etc.
        #[serde(default)]
        state: Option<ToolState>,
    },
    #[serde(rename = "step-start")]
    StepStart {
        id: String,
        #[serde(rename = "sessionID", default)]
        session_id: Option<String>,
    },
    #[serde(rename = "step-finish")]
    StepFinish {
        id: String,
        #[serde(rename = "sessionID", default)]
        session_id: Option<String>,
        #[serde(default)]
        reason: Option<String>,
    },
    /// Catch-all for part types we don't process (reasoning, file, subtask, snapshot, etc.)
    #[serde(other)]
    Other,
}

/// Tool execution state. Tagged by `status` field.
///
/// OpenCode sends this as e.g.:
/// ```json
/// { "status": "running", "input": {...}, "title": "...", "time": { "start": 1234 } }
/// ```
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ToolState {
    #[serde(rename = "pending")]
    Pending {
        #[serde(default)]
        input: Option<serde_json::Value>,
    },
    #[serde(rename = "running")]
    Running {
        #[serde(default)]
        input: Option<serde_json::Value>,
        #[serde(default)]
        title: Option<String>,
        #[serde(default)]
        metadata: Option<HashMap<String, serde_json::Value>>,
    },
    #[serde(rename = "completed")]
    Completed {
        #[serde(default)]
        input: Option<serde_json::Value>,
        #[serde(default)]
        output: Option<String>,
        #[serde(default)]
        title: Option<String>,
        #[serde(default)]
        metadata: Option<HashMap<String, serde_json::Value>>,
    },
    #[serde(rename = "error")]
    Error {
        #[serde(default)]
        input: Option<serde_json::Value>,
        #[serde(default)]
        error: Option<String>,
    },
}

impl ToolState {
    /// Check if this is a running state.
    pub fn is_running(&self) -> bool {
        matches!(self, ToolState::Running { .. })
    }

    /// Check if this is a completed state.
    pub fn is_completed(&self) -> bool {
        matches!(self, ToolState::Completed { .. })
    }

    /// Check if this is an error state.
    pub fn is_error(&self) -> bool {
        matches!(self, ToolState::Error { .. })
    }

    /// Get a display-friendly status string.
    pub fn status_str(&self) -> &'static str {
        match self {
            ToolState::Pending { .. } => "pending",
            ToolState::Running { .. } => "running",
            ToolState::Completed { .. } => "completed",
            ToolState::Error { .. } => "error",
        }
    }
}

/// Session status payload.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SessionStatusPayload {
    Idle,
    Busy,
    Retry {
        #[serde(default)]
        attempt: u32,
        #[serde(default)]
        message: Option<String>,
    },
}

/// Permission request from OpenCode.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionRequest {
    pub id: String,
    #[serde(rename = "sessionID")]
    pub session_id: String,
    #[serde(default)]
    pub permission: Option<String>,
    #[serde(default)]
    pub patterns: Vec<String>,
    #[serde(default)]
    pub metadata: HashMap<String, serde_json::Value>,
}

/// Question request from OpenCode.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QuestionRequest {
    pub id: String,
    #[serde(rename = "sessionID")]
    pub session_id: String,
    #[serde(default)]
    pub questions: Vec<QuestionInfo>,
}

/// Individual question within a question request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuestionInfo {
    #[serde(default)]
    pub question: Option<String>,
    #[serde(default)]
    pub header: Option<String>,
    #[serde(default)]
    pub options: Vec<QuestionOption>,
}

/// An option within a question.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuestionOption {
    pub label: String,
    #[serde(default)]
    pub description: Option<String>,
}

// -- OpenCode server config injected via env --

/// Configuration passed to OpenCode via `OPENCODE_CONFIG_CONTENT` env var.
#[derive(Debug, Serialize)]
pub struct OpenCodeEnvConfig {
    #[serde(rename = "$schema")]
    pub schema: String,
    pub lsp: bool,
    pub formatter: bool,
    pub permission: OpenCodePermissions,
}

/// Permission settings for headless OpenCode operation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OpenCodePermissions {
    pub edit: String,
    pub bash: String,
    #[serde(default = "default_webfetch_permission")]
    pub webfetch: String,
}

fn default_webfetch_permission() -> String {
    "allow".to_string()
}

impl Default for OpenCodePermissions {
    fn default() -> Self {
        Self {
            edit: "allow".to_string(),
            bash: "allow".to_string(),
            webfetch: "allow".to_string(),
        }
    }
}

impl OpenCodeEnvConfig {
    /// Build the config JSON that gets passed as `OPENCODE_CONFIG_CONTENT`.
    pub fn new(permissions: &OpenCodePermissions) -> Self {
        Self {
            schema: "https://opencode.ai/config.json".to_string(),
            lsp: false,
            formatter: false,
            permission: permissions.clone(),
        }
    }
}
