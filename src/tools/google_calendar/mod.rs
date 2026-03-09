//! Google Calendar tool integration.
//!
//! Provides tools for interacting with Google Calendar API v3, including
//! listing events, creating/updating/deleting events, listing calendars,
//! finding free time, and responding to event invitations.

pub mod create_event;
pub mod delete_event;
pub mod find_free_time;
pub mod list_calendars;
pub mod list_events;
pub mod respond_event;
pub mod update_event;

pub use create_event::GoogleCalendarCreateEventTool;
pub use delete_event::GoogleCalendarDeleteEventTool;
pub use find_free_time::GoogleCalendarFreeTimeTool;
pub use list_calendars::GoogleCalendarListCalendarsTool;
pub use list_events::GoogleCalendarListEventsTool;
pub use respond_event::GoogleCalendarRespondEventTool;
pub use update_event::GoogleCalendarUpdateEventTool;

use crate::config::GoogleCalendarConfig;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::RwLock;

/// Base URL for Google Calendar API v3.
pub const GOOGLE_CALENDAR_API_BASE: &str = "https://www.googleapis.com/calendar/v3";

// ---------------------------------------------------------------------------
// Error
// ---------------------------------------------------------------------------

/// Error type for Google Calendar operations.
#[derive(Debug, thiserror::Error)]
pub enum GoogleCalendarError {
    #[error("Google Calendar request failed: {0}")]
    RequestFailed(String),

    #[error("Failed to parse Google Calendar response: {0}")]
    InvalidResponse(String),

    #[error("Rate limited by Google Calendar API")]
    RateLimited,

    #[error("Failed to refresh access token: {0}")]
    TokenRefreshFailed(String),
}

// ---------------------------------------------------------------------------
// Shared output types
// ---------------------------------------------------------------------------

/// A Google Calendar event.
#[derive(Debug, Serialize, Deserialize)]
pub struct CalendarEvent {
    /// Unique event identifier.
    pub id: String,
    /// Short summary / title of the event.
    pub summary: Option<String>,
    /// Longer description of the event.
    pub description: Option<String>,
    /// When the event starts.
    pub start: Option<EventDateTime>,
    /// When the event ends.
    pub end: Option<EventDateTime>,
    /// Physical location or meeting room.
    pub location: Option<String>,
    /// List of attendees.
    pub attendees: Option<Vec<Attendee>>,
    /// Event status (e.g. "confirmed", "tentative", "cancelled").
    pub status: Option<String>,
    /// Link to view the event in Google Calendar.
    #[serde(rename = "htmlLink")]
    pub html_link: Option<String>,
}

/// Date/time representation used by the Google Calendar API.
///
/// Exactly one of `date_time` (for timed events) or `date` (for all-day
/// events) will be set.
#[derive(Debug, Serialize, Deserialize)]
pub struct EventDateTime {
    /// Combined date-time value (RFC 3339), for timed events.
    #[serde(rename = "dateTime")]
    pub date_time: Option<String>,
    /// Date value (yyyy-MM-dd), for all-day events.
    pub date: Option<String>,
    /// IANA time zone (e.g. "Europe/Prague").
    #[serde(rename = "timeZone")]
    pub time_zone: Option<String>,
}

/// An event attendee.
#[derive(Debug, Serialize, Deserialize)]
pub struct Attendee {
    /// E-mail address of the attendee.
    pub email: Option<String>,
    /// Human-readable display name.
    #[serde(rename = "displayName")]
    pub display_name: Option<String>,
    /// RSVP status: "needsAction", "declined", "tentative", or "accepted".
    #[serde(rename = "responseStatus")]
    pub response_status: Option<String>,
    /// Whether this attendee entry represents the calendar owner.
    #[serde(default, rename = "self")]
    pub is_self: bool,
}

// ---------------------------------------------------------------------------
// Token manager
// ---------------------------------------------------------------------------

/// Cached OAuth2 access token with its expiry instant.
struct CachedToken {
    access_token: String,
    expires_at: std::time::Instant,
}

impl Default for CachedToken {
    fn default() -> Self {
        Self {
            access_token: String::new(),
            // Already expired so the first call always refreshes.
            expires_at: std::time::Instant::now(),
        }
    }
}

/// Response shape returned by the Google OAuth2 token endpoint.
#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    expires_in: u64,
}

/// Thread-safe manager that caches and refreshes Google OAuth2 access tokens.
pub struct TokenManager {
    client_id: String,
    client_secret: String,
    refresh_token: String,
    cached: RwLock<CachedToken>,
}

impl TokenManager {
    /// Create a new `TokenManager` from OAuth2 credentials.
    fn new(client_id: String, client_secret: String, refresh_token: String) -> Self {
        Self {
            client_id,
            client_secret,
            refresh_token,
            cached: RwLock::new(CachedToken::default()),
        }
    }

    /// Return a valid access token, refreshing it if necessary.
    ///
    /// The token is considered stale 60 seconds before its actual expiry to
    /// avoid using a token that expires mid-request.
    pub async fn get_access_token(
        &self,
        client: &reqwest::Client,
    ) -> Result<String, GoogleCalendarError> {
        // Fast path: return the cached token if still valid.
        {
            let cached = self.cached.read().await;
            if !cached.access_token.is_empty()
                && cached.expires_at
                    > std::time::Instant::now() + std::time::Duration::from_secs(60)
            {
                return Ok(cached.access_token.clone());
            }
        }

        // Slow path: refresh the token.
        let response = client
            .post("https://oauth2.googleapis.com/token")
            .form(&[
                ("client_id", self.client_id.as_str()),
                ("client_secret", self.client_secret.as_str()),
                ("refresh_token", self.refresh_token.as_str()),
                ("grant_type", "refresh_token"),
            ])
            .send()
            .await
            .map_err(|e| GoogleCalendarError::TokenRefreshFailed(e.to_string()))?;

        let status = response.status();
        if !status.is_success() {
            let body = response
                .text()
                .await
                .unwrap_or_else(|_| "failed to read response body".into());
            return Err(GoogleCalendarError::TokenRefreshFailed(format!(
                "HTTP {status}: {body}"
            )));
        }

        let token_response: TokenResponse = response
            .json()
            .await
            .map_err(|e| GoogleCalendarError::TokenRefreshFailed(e.to_string()))?;

        let expires_at =
            std::time::Instant::now() + std::time::Duration::from_secs(token_response.expires_in);

        let access_token = token_response.access_token.clone();

        // Update the cache.
        {
            let mut cached = self.cached.write().await;
            cached.access_token = token_response.access_token;
            cached.expires_at = expires_at;
        }

        Ok(access_token)
    }
}

// ---------------------------------------------------------------------------
// Client
// ---------------------------------------------------------------------------

/// HTTP client for the Google Calendar API.
///
/// Wraps a `reqwest::Client` with automatic OAuth2 token management and
/// standard error handling (rate-limit detection, status code checks).
pub struct GoogleCalendarClient {
    client: reqwest::Client,
    token_manager: Arc<TokenManager>,
    default_calendar_id: String,
}

impl std::fmt::Debug for GoogleCalendarClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GoogleCalendarClient")
            .field("default_calendar_id", &self.default_calendar_id)
            .finish_non_exhaustive()
    }
}

impl GoogleCalendarClient {
    /// Create a new `GoogleCalendarClient` from the provided configuration.
    pub fn new(config: &GoogleCalendarConfig) -> Self {
        let client = reqwest::Client::builder()
            .gzip(true)
            .build()
            .expect("hardcoded reqwest client config");

        let token_manager = Arc::new(TokenManager::new(
            config.client_id.clone(),
            config.client_secret.clone(),
            config.refresh_token.clone(),
        ));

        Self {
            client,
            token_manager,
            default_calendar_id: config.default_calendar_id.clone(),
        }
    }

    /// Return the default calendar ID configured for this client.
    pub fn default_calendar_id(&self) -> &str {
        &self.default_calendar_id
    }

    /// Build an authenticated `RequestBuilder` for the given HTTP method and URL.
    ///
    /// The returned builder already has the `Authorization: Bearer <token>` header
    /// set. Callers can chain additional query parameters, headers, or a body
    /// before sending.
    pub async fn request(
        &self,
        method: reqwest::Method,
        url: &str,
    ) -> Result<reqwest::RequestBuilder, GoogleCalendarError> {
        let token = self.token_manager.get_access_token(&self.client).await?;

        Ok(self.client.request(method, url).bearer_auth(token))
    }

    /// Send an already-built request, handling rate limiting and error status codes.
    pub async fn send_request(
        &self,
        request: reqwest::RequestBuilder,
    ) -> Result<reqwest::Response, GoogleCalendarError> {
        let response = request
            .send()
            .await
            .map_err(|e| GoogleCalendarError::RequestFailed(e.to_string()))?;

        let status = response.status();

        if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
            return Err(GoogleCalendarError::RateLimited);
        }

        if !status.is_success() {
            let body = response
                .text()
                .await
                .unwrap_or_else(|_| "failed to read response body".into());
            return Err(GoogleCalendarError::RequestFailed(format!(
                "HTTP {status}: {body}"
            )));
        }

        Ok(response)
    }
}
