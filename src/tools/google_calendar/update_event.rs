//! Tool for updating events in a Google Calendar.

use super::{CalendarEvent, GOOGLE_CALENDAR_API_BASE, GoogleCalendarClient, GoogleCalendarError};
use rig::completion::ToolDefinition;
use rig::tool::Tool;
use schemars::JsonSchema;
use serde::Deserialize;
use std::sync::Arc;

/// Tool for updating events in a Google Calendar.
#[derive(Debug, Clone)]
pub struct GoogleCalendarUpdateEventTool {
    client: Arc<GoogleCalendarClient>,
}

impl GoogleCalendarUpdateEventTool {
    pub fn new(client: Arc<GoogleCalendarClient>) -> Self {
        Self { client }
    }
}

/// Arguments for updating a calendar event.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct UpdateEventArgs {
    /// Calendar ID containing the event. Defaults to the configured primary calendar.
    pub calendar_id: Option<String>,
    /// The unique identifier of the event to update.
    pub event_id: String,
    /// New summary / title for the event.
    pub summary: Option<String>,
    /// New description for the event.
    pub description: Option<String>,
    /// New start time as an RFC 3339 timestamp (e.g. "2026-03-10T09:00:00Z").
    pub start: Option<String>,
    /// New end time as an RFC 3339 timestamp (e.g. "2026-03-10T10:00:00Z").
    pub end: Option<String>,
    /// New physical location or meeting room.
    pub location: Option<String>,
    /// New list of attendee email addresses (replaces existing attendees).
    pub attendees: Option<Vec<String>>,
}

impl Tool for GoogleCalendarUpdateEventTool {
    const NAME: &'static str = "google_calendar_update_event";

    type Error = GoogleCalendarError;
    type Args = UpdateEventArgs;
    type Output = CalendarEvent;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: crate::prompts::text::get("tools/google_calendar_update_event")
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "calendar_id": {
                        "type": "string",
                        "description": "Calendar ID containing the event. Defaults to the configured primary calendar."
                    },
                    "event_id": {
                        "type": "string",
                        "description": "The unique identifier of the event to update."
                    },
                    "summary": {
                        "type": "string",
                        "description": "New summary / title for the event."
                    },
                    "description": {
                        "type": "string",
                        "description": "New description for the event."
                    },
                    "start": {
                        "type": "string",
                        "description": "New start time as an RFC 3339 timestamp (e.g. \"2026-03-10T09:00:00Z\")."
                    },
                    "end": {
                        "type": "string",
                        "description": "New end time as an RFC 3339 timestamp (e.g. \"2026-03-10T10:00:00Z\")."
                    },
                    "location": {
                        "type": "string",
                        "description": "New physical location or meeting room."
                    },
                    "attendees": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "New list of attendee email addresses (replaces existing attendees)."
                    }
                },
                "required": ["event_id"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let calendar_id = args
            .calendar_id
            .as_deref()
            .unwrap_or_else(|| self.client.default_calendar_id());

        let url = format!(
            "{}/calendars/{}/events/{}",
            GOOGLE_CALENDAR_API_BASE, calendar_id, args.event_id
        );

        let mut body = serde_json::Map::new();

        if let Some(summary) = &args.summary {
            body.insert("summary".into(), serde_json::json!(summary));
        }
        if let Some(desc) = &args.description {
            body.insert("description".into(), serde_json::json!(desc));
        }
        if let Some(start) = &args.start {
            body.insert("start".into(), serde_json::json!({"dateTime": start}));
        }
        if let Some(end) = &args.end {
            body.insert("end".into(), serde_json::json!({"dateTime": end}));
        }
        if let Some(loc) = &args.location {
            body.insert("location".into(), serde_json::json!(loc));
        }
        if let Some(attendees) = &args.attendees {
            body.insert(
                "attendees".into(),
                serde_json::json!(
                    attendees
                        .iter()
                        .map(|e| serde_json::json!({"email": e}))
                        .collect::<Vec<_>>()
                ),
            );
        }

        let request = self
            .client
            .request(reqwest::Method::PATCH, &url)
            .await?
            .json(&serde_json::Value::Object(body));

        let response = self.client.send_request(request).await?;

        let event: CalendarEvent = response
            .json()
            .await
            .map_err(|e| GoogleCalendarError::InvalidResponse(e.to_string()))?;

        Ok(event)
    }
}
