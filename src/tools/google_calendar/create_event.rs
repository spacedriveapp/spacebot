//! Tool for creating events in a Google Calendar.

use super::{CalendarEvent, GOOGLE_CALENDAR_API_BASE, GoogleCalendarClient, GoogleCalendarError};
use rig::completion::ToolDefinition;
use rig::tool::Tool;
use schemars::JsonSchema;
use serde::Deserialize;
use std::sync::Arc;

/// Tool for creating events in a Google Calendar.
#[derive(Debug, Clone)]
pub struct GoogleCalendarCreateEventTool {
    client: Arc<GoogleCalendarClient>,
}

impl GoogleCalendarCreateEventTool {
    pub fn new(client: Arc<GoogleCalendarClient>) -> Self {
        Self { client }
    }
}

/// Arguments for creating a calendar event.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct CreateEventArgs {
    /// Calendar ID to create the event in. Defaults to the configured primary calendar.
    pub calendar_id: Option<String>,
    /// Short summary / title of the event.
    pub summary: String,
    /// Longer description of the event.
    pub description: Option<String>,
    /// Event start time as an RFC 3339 timestamp (e.g. "2026-03-10T09:00:00Z").
    pub start: String,
    /// Event end time as an RFC 3339 timestamp (e.g. "2026-03-10T10:00:00Z").
    pub end: String,
    /// Physical location or meeting room.
    pub location: Option<String>,
    /// List of attendee email addresses.
    pub attendees: Option<Vec<String>>,
    /// RRULE strings for recurring events (e.g. ["RRULE:FREQ=WEEKLY;COUNT=10"]).
    pub recurrence: Option<Vec<String>>,
}

impl Tool for GoogleCalendarCreateEventTool {
    const NAME: &'static str = "google_calendar_create_event";

    type Error = GoogleCalendarError;
    type Args = CreateEventArgs;
    type Output = CalendarEvent;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: crate::prompts::text::get("tools/google_calendar_create_event")
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "calendar_id": {
                        "type": "string",
                        "description": "Calendar ID to create the event in. Defaults to the configured primary calendar."
                    },
                    "summary": {
                        "type": "string",
                        "description": "Short summary / title of the event."
                    },
                    "description": {
                        "type": "string",
                        "description": "Longer description of the event."
                    },
                    "start": {
                        "type": "string",
                        "description": "Event start time as an RFC 3339 timestamp (e.g. \"2026-03-10T09:00:00Z\")."
                    },
                    "end": {
                        "type": "string",
                        "description": "Event end time as an RFC 3339 timestamp (e.g. \"2026-03-10T10:00:00Z\")."
                    },
                    "location": {
                        "type": "string",
                        "description": "Physical location or meeting room."
                    },
                    "attendees": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "List of attendee email addresses."
                    },
                    "recurrence": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "RRULE strings for recurring events (e.g. [\"RRULE:FREQ=WEEKLY;COUNT=10\"])."
                    }
                },
                "required": ["summary", "start", "end"]
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

        let mut body = serde_json::json!({
            "summary": args.summary,
            "start": { "dateTime": args.start },
            "end": { "dateTime": args.end },
        });

        if let Some(description) = &args.description {
            body["description"] = serde_json::json!(description);
        }
        if let Some(location) = &args.location {
            body["location"] = serde_json::json!(location);
        }
        if let Some(attendees) = &args.attendees {
            body["attendees"] = serde_json::json!(
                attendees
                    .iter()
                    .map(|e| serde_json::json!({"email": e}))
                    .collect::<Vec<_>>()
            );
        }
        if let Some(recurrence) = &args.recurrence {
            body["recurrence"] = serde_json::json!(recurrence);
        }

        let request = self
            .client
            .request(reqwest::Method::POST, &url)
            .await?
            .json(&body);

        let response = self.client.send_request(request).await?;

        let event: CalendarEvent = response
            .json()
            .await
            .map_err(|e| GoogleCalendarError::InvalidResponse(e.to_string()))?;

        Ok(event)
    }
}
