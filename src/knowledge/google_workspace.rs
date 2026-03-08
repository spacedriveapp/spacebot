//! Normalization helpers for Google Workspace bridge outputs.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::knowledge::registry::{
    GOOGLE_WORKSPACE_CALENDAR_SOURCE_ID, GOOGLE_WORKSPACE_DRIVE_SOURCE_ID,
    GOOGLE_WORKSPACE_GMAIL_SOURCE_ID,
};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GoogleWorkspaceFamily {
    Drive,
    Gmail,
    Calendar,
    Unknown,
}

impl GoogleWorkspaceFamily {
    pub fn from_method(method: &str) -> Self {
        let normalized = method.trim().to_lowercase();
        if normalized.starts_with("drive.") {
            return Self::Drive;
        }
        if normalized.starts_with("gmail.") {
            return Self::Gmail;
        }
        if normalized.starts_with("calendar.") {
            return Self::Calendar;
        }
        Self::Unknown
    }

    pub fn source_id(self) -> Option<&'static str> {
        match self {
            Self::Drive => Some(GOOGLE_WORKSPACE_DRIVE_SOURCE_ID),
            Self::Gmail => Some(GOOGLE_WORKSPACE_GMAIL_SOURCE_ID),
            Self::Calendar => Some(GOOGLE_WORKSPACE_CALENDAR_SOURCE_ID),
            Self::Unknown => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Drive => "Google Workspace Drive",
            Self::Gmail => "Google Workspace Gmail",
            Self::Calendar => "Google Workspace Calendar",
            Self::Unknown => "Google Workspace",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GoogleWorkspaceSchemaSummary {
    pub family: GoogleWorkspaceFamily,
    pub source_id: String,
    pub source_label: String,
    pub method: String,
    pub description: String,
    pub required_parameter_count: usize,
    pub optional_parameter_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GoogleWorkspaceDryRunSummary {
    pub family: GoogleWorkspaceFamily,
    pub source_id: String,
    pub source_label: String,
    pub method: String,
    pub dry_run: bool,
    pub http_method: String,
    pub url: String,
    pub query_param_keys: Vec<String>,
    pub body_key_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GoogleWorkspaceAuthStatusSummary {
    pub authenticated: bool,
    pub auth_method: String,
    pub principal: Option<String>,
    pub raw_status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GoogleWorkspaceNormalizedSourceStatus {
    pub source_id: String,
    pub source_label: String,
    pub message: String,
}

#[derive(Debug, thiserror::Error)]
pub enum GoogleWorkspaceBridgeError {
    #[error("invalid JSON from google workspace bridge: {0}")]
    InvalidJson(#[from] serde_json::Error),
    #[error("google workspace bridge output is missing required field: {0}")]
    MissingField(&'static str),
    #[error("google workspace bridge output format is not supported")]
    UnsupportedFormat,
}

const DESCRIPTION_PLACEHOLDER: &str =
    "Google Workspace bridge adapter is configured but the method metadata is incomplete.";

pub fn normalize_schema_output(
    raw: &str,
) -> Result<GoogleWorkspaceSchemaSummary, GoogleWorkspaceBridgeError> {
    let value: Value = serde_json::from_str(raw)?;
    let object = value
        .as_object()
        .ok_or(GoogleWorkspaceBridgeError::UnsupportedFormat)?;

    let method = string_field(object, &["method", "id"])
        .ok_or(GoogleWorkspaceBridgeError::MissingField("method"))?;

    let family = GoogleWorkspaceFamily::from_method(&method);
    let source_id = family.source_id().unwrap_or("google_workspace_unknown");

    let description = string_field(object, &["description", "summary"])
        .unwrap_or_else(|| DESCRIPTION_PLACEHOLDER.to_string())
        .to_string();

    let parameters = object_array(object, "parameters").unwrap_or(&[]);
    let mut required_parameter_count = 0usize;
    let mut optional_parameter_count = 0usize;

    for value in parameters {
        let parameter = value
            .as_object()
            .ok_or(GoogleWorkspaceBridgeError::UnsupportedFormat)?;
        if bool_field(parameter, &["required"]).unwrap_or(false) {
            required_parameter_count += 1;
        } else {
            optional_parameter_count += 1;
        }
    }

    Ok(GoogleWorkspaceSchemaSummary {
        family,
        source_id: source_id.to_string(),
        source_label: family.label().to_string(),
        method,
        description,
        required_parameter_count,
        optional_parameter_count,
    })
}

pub fn normalize_dry_run_output(
    raw: &str,
) -> Result<GoogleWorkspaceDryRunSummary, GoogleWorkspaceBridgeError> {
    let value: Value = serde_json::from_str(raw)?;
    let object = value
        .as_object()
        .ok_or(GoogleWorkspaceBridgeError::UnsupportedFormat)?;

    let method = string_field(object, &["method", "id"])
        .ok_or(GoogleWorkspaceBridgeError::MissingField("method"))?;
    let family = GoogleWorkspaceFamily::from_method(&method);
    let source_id = family.source_id().unwrap_or("google_workspace_unknown");

    let dry_run = bool_field(object, &["dry_run"]).unwrap_or(false);
    let http_method =
        string_field(object, &["httpMethod", "http_method"]).unwrap_or_else(|| "GET".to_string());
    let url = string_field(object, &["url", "endpoint"]).unwrap_or_default();
    if url.is_empty() {
        return Err(GoogleWorkspaceBridgeError::MissingField("url"));
    }

    let body_key_count = match object.get("body") {
        Some(value) if !value.is_null() => {
            value.as_object().map(|object| object.len()).unwrap_or(0)
        }
        _ => 0,
    };

    let query_param_keys = object
        .get("queryParams")
        .or_else(|| object.get("query_params"))
        .and_then(Value::as_object)
        .map(|query| {
            let mut keys = query.keys().cloned().collect::<Vec<_>>();
            keys.sort_unstable();
            keys
        })
        .unwrap_or_default();

    Ok(GoogleWorkspaceDryRunSummary {
        family,
        source_id: source_id.to_string(),
        source_label: family.label().to_string(),
        method,
        dry_run,
        http_method,
        url,
        query_param_keys,
        body_key_count,
    })
}

pub fn normalize_auth_status_output(
    raw: &str,
) -> Result<GoogleWorkspaceAuthStatusSummary, GoogleWorkspaceBridgeError> {
    let value: Value = serde_json::from_str(raw)?;
    let object = value
        .as_object()
        .ok_or(GoogleWorkspaceBridgeError::UnsupportedFormat)?;

    let authenticated = bool_field(object, &["authenticated"]).unwrap_or(false);
    let auth_method = string_field(object, &["authMethod", "auth_method", "method"])
        .unwrap_or_else(|| "unknown".to_string());
    let principal = string_field(object, &["account", "email", "user", "principal"]);
    let raw_status = string_field(object, &["status", "state"]).unwrap_or_else(|| {
        if authenticated {
            "authenticated".to_string()
        } else {
            "not_authenticated".to_string()
        }
    });

    Ok(GoogleWorkspaceAuthStatusSummary {
        authenticated,
        auth_method,
        principal,
        raw_status,
    })
}

pub fn status_message_for_source(
    source_id: &str,
    auth_status: Option<&GoogleWorkspaceAuthStatusSummary>,
) -> String {
    let (source_label, family_unknown) = match source_id {
        GOOGLE_WORKSPACE_DRIVE_SOURCE_ID => ("Google Workspace Drive", false),
        GOOGLE_WORKSPACE_GMAIL_SOURCE_ID => ("Google Workspace Gmail", false),
        GOOGLE_WORKSPACE_CALENDAR_SOURCE_ID => ("Google Workspace Calendar", false),
        _ => ("Google Workspace", true),
    };

    let auth_prefix = match auth_status {
        Some(status) if status.authenticated => {
            let principal = status
                .principal
                .as_deref()
                .map(|value| format!(" [{value}]"))
                .unwrap_or_else(String::new);
            format!("credentials present{principal}")
        }
        Some(status) => format!(
            "not yet authenticated via {} ({})",
            status.auth_method, status.raw_status
        ),
        None => "bridge adapter metadata is not resolved in this execution context".to_string(),
    };

    if family_unknown {
        format!("{source_label} source is not a known source family ({auth_prefix}).")
    } else {
        format!(
            "{source_label} source is available through the bridge adapter but currently unavailable to this call ({auth_prefix})."
        )
    }
}

pub fn normalized_source_statuses_from_auth(
    auth_status: &GoogleWorkspaceAuthStatusSummary,
) -> Vec<GoogleWorkspaceNormalizedSourceStatus> {
    let mut statuses = Vec::with_capacity(3);
    for source_id in [
        GOOGLE_WORKSPACE_DRIVE_SOURCE_ID,
        GOOGLE_WORKSPACE_GMAIL_SOURCE_ID,
        GOOGLE_WORKSPACE_CALENDAR_SOURCE_ID,
    ] {
        let family = match source_id {
            GOOGLE_WORKSPACE_DRIVE_SOURCE_ID => GoogleWorkspaceFamily::Drive,
            GOOGLE_WORKSPACE_GMAIL_SOURCE_ID => GoogleWorkspaceFamily::Gmail,
            GOOGLE_WORKSPACE_CALENDAR_SOURCE_ID => GoogleWorkspaceFamily::Calendar,
            _ => GoogleWorkspaceFamily::Unknown,
        };

        statuses.push(GoogleWorkspaceNormalizedSourceStatus {
            source_id: source_id.to_string(),
            source_label: family.label().to_string(),
            message: if auth_status.authenticated {
                format!(
                    "{} source is authenticated as part of the bridge adapter ({}).",
                    family.label(),
                    auth_status.auth_method,
                )
            } else {
                "Bridge adapter is not yet authenticated; run Google Workspace auth flows and retry.".to_string()
            },
        });
    }

    statuses
}

fn bool_field(object: &serde_json::Map<String, Value>, keys: &[&str]) -> Option<bool> {
    for key in keys {
        if let Some(value) = object.get(*key)
            && let Some(boolean) = value.as_bool()
        {
            return Some(boolean);
        }
    }
    None
}

fn string_field(object: &serde_json::Map<String, Value>, keys: &[&str]) -> Option<String> {
    for key in keys {
        if let Some(value) = object.get(*key)
            && let Some(value) = value.as_str()
        {
            return Some(value.to_string());
        }
    }
    None
}

fn object_array<'a>(object: &'a serde_json::Map<String, Value>, key: &str) -> Option<&'a [Value]> {
    object
        .get(key)
        .and_then(Value::as_array)
        .map(|value| &value[..])
}

#[cfg(test)]
mod tests {
    use super::{
        GoogleWorkspaceBridgeError, GoogleWorkspaceNormalizedSourceStatus,
        normalize_auth_status_output, normalize_dry_run_output, normalize_schema_output,
    };
    use super::{normalized_source_statuses_from_auth, status_message_for_source};

    const SCHEMA_FIXTURE: &str = r#"
    {
      "service": "drive",
      "id": "drive.files.list",
      "description": "List files and folders.",
      "parameters": [
        {
          "name": "q",
          "type": "string",
          "required": false
        },
        {
          "name": "pageSize",
          "type": "integer",
          "required": false
        },
        {
          "name": "includeItemsFromAllDrives",
          "type": "boolean",
          "required": false
        }
      ]
    }
    "#;

    const DRYRUN_FIXTURE: &str = r#"
    {
      "method": "gmail.users.messages.list",
      "dry_run": true,
      "httpMethod": "GET",
      "url": "https://gmail.googleapis.com/gmail/v1/users/me/messages",
      "queryParams": {
        "q": "from:alice@example.com",
        "maxResults": 10
      },
      "body": {}
    }
    "#;

    const AUTH_FIXTURE: &str = r#"
    {
      "authenticated": true,
      "status": "ok",
      "method": "oauth",
      "account": "alice@example.com"
    }
    "#;

    const BROKEN_FIXTURE: &str = r#"
    {
      "dry_run": true
    }
    "#;

    #[test]
    fn normalizes_gws_schema_output_into_family_metadata() {
        let summary =
            normalize_schema_output(SCHEMA_FIXTURE).expect("schema fixture should normalize");

        assert_eq!(summary.method, "drive.files.list");
        assert_eq!(summary.source_id, "google_workspace_drive");
        assert_eq!(summary.source_label, "Google Workspace Drive");
        assert_eq!(summary.required_parameter_count, 0);
        assert_eq!(summary.optional_parameter_count, 3);
    }

    #[test]
    fn normalizes_gws_dry_run_output_into_native_summary() {
        let summary =
            normalize_dry_run_output(DRYRUN_FIXTURE).expect("dry run fixture should normalize");

        assert_eq!(summary.method, "gmail.users.messages.list");
        assert_eq!(summary.http_method, "GET");
        assert_eq!(
            summary.url,
            "https://gmail.googleapis.com/gmail/v1/users/me/messages"
        );
        assert_eq!(summary.body_key_count, 0);
        assert!(summary.query_param_keys.contains(&"maxResults".to_string()));
        assert_eq!(summary.source_id, "google_workspace_gmail");
    }

    #[test]
    fn normalizes_gws_auth_status_without_leaking_raw_cli_semantics() {
        let auth =
            normalize_auth_status_output(AUTH_FIXTURE).expect("auth fixture should normalize");

        assert!(auth.authenticated);
        assert_eq!(auth.auth_method, "oauth");
        assert_eq!(auth.principal.as_deref(), Some("alice@example.com"));
        assert_eq!(auth.raw_status, "ok");

        let status = status_message_for_source("google_workspace_gmail", Some(&auth));
        assert!(status.contains("Google Workspace Gmail"));
        assert!(status.contains("credentials present"));
        assert!(!status.contains("gws"));
    }

    #[test]
    fn structured_error_for_missing_schema_method() {
        let error = normalize_dry_run_output(BROKEN_FIXTURE)
            .expect_err("dry run fixture missing method should fail");

        match error {
            GoogleWorkspaceBridgeError::MissingField(field) => {
                assert_eq!(field, "method");
            }
            other => panic!("unexpected error: {other}"),
        }
    }

    #[test]
    fn can_project_auth_status_to_normalized_source_statuses() {
        let auth =
            normalize_auth_status_output(AUTH_FIXTURE).expect("auth fixture should normalize");
        let statuses: Vec<GoogleWorkspaceNormalizedSourceStatus> =
            normalized_source_statuses_from_auth(&auth);

        assert_eq!(statuses.len(), 3);
        assert!(
            statuses
                .iter()
                .any(|status| status.source_id == "google_workspace_drive")
        );
        assert!(
            statuses
                .iter()
                .all(|status| !status.message.contains("gws"))
        );
    }
}
