//! Tool for finding free/busy time across Google Calendars.

use super::{GOOGLE_CALENDAR_API_BASE, GoogleCalendarClient, GoogleCalendarError};
use rig::completion::ToolDefinition;
use rig::tool::Tool;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// Tool for querying free/busy information from Google Calendar.
#[derive(Debug, Clone)]
pub struct GoogleCalendarFreeTimeTool {
    client: Arc<GoogleCalendarClient>,
}

impl GoogleCalendarFreeTimeTool {
    pub fn new(client: Arc<GoogleCalendarClient>) -> Self {
        Self { client }
    }
}

/// Arguments for finding free/busy time.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct FindFreeTimeArgs {
    /// Calendar IDs to check. Defaults to `["primary"]` if not specified.
    pub calendar_ids: Option<Vec<String>>,
    /// Start of the time range to query, as an RFC 3339 timestamp
    /// (e.g. "2026-03-10T08:00:00Z").
    pub time_min: String,
    /// End of the time range to query, as an RFC 3339 timestamp.
    pub time_max: String,
}

/// Output from the free/busy query.
#[derive(Debug, Serialize)]
pub struct FindFreeTimeOutput {
    /// Busy periods found across the queried calendars.
    pub busy_periods: Vec<BusyPeriod>,
}

/// A single busy period on a calendar.
#[derive(Debug, Serialize)]
pub struct BusyPeriod {
    /// The calendar ID this busy period belongs to.
    pub calendar_id: String,
    /// Start of the busy period (RFC 3339).
    pub start: String,
    /// End of the busy period (RFC 3339).
    pub end: String,
}

/// Private response types for the Google Calendar freeBusy endpoint.
#[derive(Debug, Deserialize)]
struct FreeBusyResponse {
    #[serde(default)]
    calendars: std::collections::HashMap<String, FreeBusyCalendar>,
}

#[derive(Debug, Deserialize)]
struct FreeBusyCalendar {
    #[serde(default)]
    busy: Vec<FreeBusyPeriod>,
}

#[derive(Debug, Deserialize)]
struct FreeBusyPeriod {
    start: String,
    end: String,
}

impl Tool for GoogleCalendarFreeTimeTool {
    const NAME: &'static str = "google_calendar_find_free_time";

    type Error = GoogleCalendarError;
    type Args = FindFreeTimeArgs;
    type Output = FindFreeTimeOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: crate::prompts::text::get("tools/google_calendar_find_free_time")
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "calendar_ids": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Calendar IDs to check for busy time. Defaults to [\"primary\"] if not specified."
                    },
                    "time_min": {
                        "type": "string",
                        "description": "Start of the time range to query, as an RFC 3339 timestamp (e.g. \"2026-03-10T08:00:00Z\")."
                    },
                    "time_max": {
                        "type": "string",
                        "description": "End of the time range to query, as an RFC 3339 timestamp."
                    }
                },
                "required": ["time_min", "time_max"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let calendar_ids = args
            .calendar_ids
            .unwrap_or_else(|| vec!["primary".to_string()]);

        let items: Vec<serde_json::Value> = calendar_ids
            .iter()
            .map(|id| serde_json::json!({ "id": id }))
            .collect();

        let body = serde_json::json!({
            "timeMin": args.time_min,
            "timeMax": args.time_max,
            "items": items,
        });

        let url = format!("{}/freeBusy", GOOGLE_CALENDAR_API_BASE);

        let request = self
            .client
            .request(reqwest::Method::POST, &url)
            .await?
            .json(&body);

        let response = self.client.send_request(request).await?;

        let api_response: FreeBusyResponse = response
            .json()
            .await
            .map_err(|e| GoogleCalendarError::InvalidResponse(e.to_string()))?;

        let mut busy_periods = Vec::new();

        for (calendar_id, calendar_data) in api_response.calendars {
            for period in calendar_data.busy {
                busy_periods.push(BusyPeriod {
                    calendar_id: calendar_id.clone(),
                    start: period.start,
                    end: period.end,
                });
            }
        }

        Ok(FindFreeTimeOutput { busy_periods })
    }
}
