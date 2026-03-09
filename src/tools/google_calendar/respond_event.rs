//! Tool for responding to an event invitation on Google Calendar.

use super::{CalendarEvent, GOOGLE_CALENDAR_API_BASE, GoogleCalendarClient, GoogleCalendarError};
use rig::completion::ToolDefinition;
use rig::tool::Tool;
use schemars::JsonSchema;
use serde::Deserialize;
use std::sync::Arc;

/// Tool for responding to a Google Calendar event invitation (accept, decline, tentative).
#[derive(Debug, Clone)]
pub struct GoogleCalendarRespondEventTool {
    client: Arc<GoogleCalendarClient>,
}

impl GoogleCalendarRespondEventTool {
    pub fn new(client: Arc<GoogleCalendarClient>) -> Self {
        Self { client }
    }
}

/// Arguments for responding to a calendar event invitation.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct RespondEventArgs {
    /// Calendar ID the event belongs to. Defaults to the configured primary calendar.
    pub calendar_id: Option<String>,
    /// The unique identifier of the event to respond to.
    pub event_id: String,
    /// RSVP response: one of "accepted", "declined", or "tentative".
    pub response: String,
}

impl Tool for GoogleCalendarRespondEventTool {
    const NAME: &'static str = "google_calendar_respond_event";

    type Error = GoogleCalendarError;
    type Args = RespondEventArgs;
    type Output = CalendarEvent;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: crate::prompts::text::get("tools/google_calendar_respond_event")
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "calendar_id": {
                        "type": "string",
                        "description": "Calendar ID the event belongs to. Defaults to the configured primary calendar."
                    },
                    "event_id": {
                        "type": "string",
                        "description": "The unique identifier of the event to respond to."
                    },
                    "response": {
                        "type": "string",
                        "enum": ["accepted", "declined", "tentative"],
                        "description": "RSVP response: one of \"accepted\", \"declined\", or \"tentative\"."
                    }
                },
                "required": ["event_id", "response"]
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

        // Validate response value.
        if !["accepted", "declined", "tentative"].contains(&args.response.as_str()) {
            return Err(GoogleCalendarError::RequestFailed(format!(
                "invalid response value '{}': must be one of accepted, declined, tentative",
                args.response
            )));
        }

        // Fetch current event.
        let get_request = self.client.request(reqwest::Method::GET, &url).await?;

        let response = self.client.send_request(get_request).await?;

        let mut event: CalendarEvent = response
            .json()
            .await
            .map_err(|e| GoogleCalendarError::InvalidResponse(e.to_string()))?;

        // Update the self attendee's response status.
        let mut self_attendee_found = false;
        if let Some(attendees) = &mut event.attendees {
            for attendee in attendees.iter_mut() {
                if attendee.is_self {
                    attendee.response_status = Some(args.response.clone());
                    self_attendee_found = true;
                }
            }
        }

        if !self_attendee_found {
            return Err(GoogleCalendarError::RequestFailed(
                "You are not listed as an attendee for this event.".to_string(),
            ));
        }

        // PATCH back with updated attendees.
        let patch_body = serde_json::json!({ "attendees": event.attendees });

        let patch_request = self
            .client
            .request(reqwest::Method::PATCH, &url)
            .await?
            .json(&patch_body);

        let patch_response = self.client.send_request(patch_request).await?;

        let updated_event: CalendarEvent = patch_response
            .json()
            .await
            .map_err(|e| GoogleCalendarError::InvalidResponse(e.to_string()))?;

        Ok(updated_event)
    }
}
