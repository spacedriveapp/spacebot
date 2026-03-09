//! Tool for listing events from a Google Calendar.

use super::{CalendarEvent, GOOGLE_CALENDAR_API_BASE, GoogleCalendarClient, GoogleCalendarError};
use rig::completion::ToolDefinition;
use rig::tool::Tool;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// Tool for listing events from a Google Calendar.
#[derive(Debug, Clone)]
pub struct GoogleCalendarListEventsTool {
    client: Arc<GoogleCalendarClient>,
}

impl GoogleCalendarListEventsTool {
    pub fn new(client: Arc<GoogleCalendarClient>) -> Self {
        Self { client }
    }
}

/// Arguments for listing calendar events.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ListEventsArgs {
    /// Calendar ID to list events from. Defaults to the configured primary calendar.
    pub calendar_id: Option<String>,
    /// Lower bound (inclusive) for event start time, as an RFC 3339 timestamp
    /// (e.g. "2026-03-01T00:00:00Z").
    pub time_min: Option<String>,
    /// Upper bound (exclusive) for event start time, as an RFC 3339 timestamp.
    pub time_max: Option<String>,
    /// Free-text search query to filter events.
    pub query: Option<String>,
    /// Maximum number of events to return (default 10).
    #[serde(default = "default_max_results")]
    pub max_results: u32,
}

fn default_max_results() -> u32 {
    10
}

/// Output from listing calendar events.
#[derive(Debug, Serialize)]
pub struct ListEventsOutput {
    /// The matching calendar events.
    pub events: Vec<CalendarEvent>,
    /// Number of events returned.
    pub count: usize,
}

/// Private response shape for the Google Calendar events list endpoint.
#[derive(Debug, Deserialize)]
struct ListEventsResponse {
    #[serde(default)]
    items: Vec<CalendarEvent>,
}

impl Tool for GoogleCalendarListEventsTool {
    const NAME: &'static str = "google_calendar_list_events";

    type Error = GoogleCalendarError;
    type Args = ListEventsArgs;
    type Output = ListEventsOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: crate::prompts::text::get("tools/google_calendar_list_events").to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "calendar_id": {
                        "type": "string",
                        "description": "Calendar ID to list events from. Defaults to the configured primary calendar."
                    },
                    "time_min": {
                        "type": "string",
                        "description": "Lower bound (inclusive) for event start time, as an RFC 3339 timestamp (e.g. \"2026-03-01T00:00:00Z\")."
                    },
                    "time_max": {
                        "type": "string",
                        "description": "Upper bound (exclusive) for event start time, as an RFC 3339 timestamp."
                    },
                    "query": {
                        "type": "string",
                        "description": "Free-text search query to filter events."
                    },
                    "max_results": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": 2500,
                        "default": 10,
                        "description": "Maximum number of events to return (default 10)."
                    }
                }
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let calendar_id = args
            .calendar_id
            .as_deref()
            .unwrap_or_else(|| self.client.default_calendar_id());

        let url = format!(
            "{}/calendars/{}/events",
            GOOGLE_CALENDAR_API_BASE, calendar_id
        );

        let request = self.client.request(reqwest::Method::GET, &url).await?;

        let mut request = request
            .query(&[("singleEvents", "true")])
            .query(&[("orderBy", "startTime")])
            .query(&[("maxResults", &args.max_results.to_string())]);

        if let Some(time_min) = &args.time_min {
            request = request.query(&[("timeMin", time_min)]);
        }
        if let Some(time_max) = &args.time_max {
            request = request.query(&[("timeMax", time_max)]);
        }
        if let Some(query) = &args.query {
            request = request.query(&[("q", query)]);
        }

        let response = self.client.send_request(request).await?;

        let api_response: ListEventsResponse = response
            .json()
            .await
            .map_err(|e| GoogleCalendarError::InvalidResponse(e.to_string()))?;

        let count = api_response.items.len();

        Ok(ListEventsOutput {
            events: api_response.items,
            count,
        })
    }
}
