//! Tool for listing available Google Calendars.

use super::{GOOGLE_CALENDAR_API_BASE, GoogleCalendarClient, GoogleCalendarError};
use rig::completion::ToolDefinition;
use rig::tool::Tool;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// Tool for listing available Google Calendars for the authenticated user.
#[derive(Debug, Clone)]
pub struct GoogleCalendarListCalendarsTool {
    client: Arc<GoogleCalendarClient>,
}

impl GoogleCalendarListCalendarsTool {
    pub fn new(client: Arc<GoogleCalendarClient>) -> Self {
        Self { client }
    }
}

/// Arguments for listing calendars (none required).
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ListCalendarsArgs {}

/// Output from listing calendars.
#[derive(Debug, Serialize)]
pub struct ListCalendarsOutput {
    /// The available calendars.
    pub calendars: Vec<CalendarSummary>,
}

/// Summary information about a single calendar.
#[derive(Debug, Serialize, Deserialize)]
pub struct CalendarSummary {
    /// The calendar ID.
    pub id: String,
    /// Human-readable name of the calendar.
    pub summary: Option<String>,
    /// Whether this is the user's primary calendar.
    pub primary: Option<bool>,
    /// The effective access role the authenticated user has on the calendar.
    #[serde(rename = "accessRole")]
    pub access_role: Option<String>,
}

/// Private response shape for the Google Calendar calendar list endpoint.
#[derive(Debug, Deserialize)]
struct CalendarListResponse {
    #[serde(default)]
    items: Vec<CalendarSummary>,
}

impl Tool for GoogleCalendarListCalendarsTool {
    const NAME: &'static str = "google_calendar_list_calendars";

    type Error = GoogleCalendarError;
    type Args = ListCalendarsArgs;
    type Output = ListCalendarsOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: crate::prompts::text::get("tools/google_calendar_list_calendars")
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {}
            }),
        }
    }

    async fn call(&self, _args: Self::Args) -> Result<Self::Output, Self::Error> {
        let url = format!("{}/users/me/calendarList", GOOGLE_CALENDAR_API_BASE);

        let request = self.client.request(reqwest::Method::GET, &url).await?;

        let response = self.client.send_request(request).await?;

        let api_response: CalendarListResponse = response
            .json()
            .await
            .map_err(|e| GoogleCalendarError::InvalidResponse(e.to_string()))?;

        Ok(ListCalendarsOutput {
            calendars: api_response.items,
        })
    }
}
