//! Tool for deleting an event from a Google Calendar.

use super::{GOOGLE_CALENDAR_API_BASE, GoogleCalendarClient, GoogleCalendarError};
use rig::completion::ToolDefinition;
use rig::tool::Tool;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// Tool for deleting an event from a Google Calendar.
#[derive(Debug, Clone)]
pub struct GoogleCalendarDeleteEventTool {
    client: Arc<GoogleCalendarClient>,
}

impl GoogleCalendarDeleteEventTool {
    pub fn new(client: Arc<GoogleCalendarClient>) -> Self {
        Self { client }
    }
}

/// Arguments for deleting a calendar event.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct DeleteEventArgs {
    /// Calendar ID to delete the event from. Defaults to the configured primary calendar.
    pub calendar_id: Option<String>,
    /// The unique identifier of the event to delete.
    pub event_id: String,
}

/// Output from deleting a calendar event.
#[derive(Debug, Serialize)]
pub struct DeleteEventOutput {
    /// Whether the event was successfully deleted.
    pub deleted: bool,
    /// The ID of the deleted event.
    pub event_id: String,
}

impl Tool for GoogleCalendarDeleteEventTool {
    const NAME: &'static str = "google_calendar_delete_event";

    type Error = GoogleCalendarError;
    type Args = DeleteEventArgs;
    type Output = DeleteEventOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: crate::prompts::text::get("tools/google_calendar_delete_event")
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "calendar_id": {
                        "type": "string",
                        "description": "Calendar ID to delete the event from. Defaults to the configured primary calendar."
                    },
                    "event_id": {
                        "type": "string",
                        "description": "The unique identifier of the event to delete."
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

        let request = self.client.request(reqwest::Method::DELETE, &url).await?;

        // The DELETE endpoint returns 204 No Content on success — no body to parse.
        self.client.send_request(request).await?;

        Ok(DeleteEventOutput {
            deleted: true,
            event_id: args.event_id,
        })
    }
}
