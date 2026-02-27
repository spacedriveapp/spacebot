//! Search email directly from IMAP for read-back and retrieval.

use crate::config::{Config, EmailConfig, RuntimeConfig};
use crate::messaging::email::EmailSearchQuery;
use rig::completion::ToolDefinition;
use rig::tool::Tool;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::Arc;

/// Tool for searching mailbox content through IMAP.
#[derive(Debug, Clone)]
pub struct EmailSearchTool {
    runtime_config: Arc<RuntimeConfig>,
}

impl EmailSearchTool {
    pub fn new(runtime_config: Arc<RuntimeConfig>) -> Self {
        Self { runtime_config }
    }
}

/// Error type for email_search tool.
#[derive(Debug, thiserror::Error)]
#[error("email_search failed: {0}")]
pub struct EmailSearchError(String);

/// Arguments for email_search.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct EmailSearchArgs {
    /// Full-text query against message body and headers.
    #[serde(default)]
    pub query: Option<String>,
    /// Sender filter (email or display-name fragment).
    #[serde(default)]
    pub from: Option<String>,
    /// Subject filter.
    #[serde(default)]
    pub subject: Option<String>,
    /// Optional folder list. Defaults to configured email folders.
    #[serde(default)]
    pub folders: Vec<String>,
    /// When true, restricts to unread messages.
    #[serde(default)]
    pub unread_only: bool,
    /// Search lookback window in days. Defaults to 30.
    #[serde(default)]
    pub since_days: Option<u32>,
    /// Maximum results to return (1..50). Defaults to 10.
    #[serde(default)]
    pub limit: Option<usize>,
}

/// A single email search result.
#[derive(Debug, Serialize)]
pub struct EmailSearchResult {
    pub folder: String,
    pub uid: u32,
    pub from: String,
    pub subject: String,
    pub date: Option<String>,
    pub message_id: Option<String>,
    pub body_snippet: String,
    pub attachment_names: Vec<String>,
}

/// Output for email_search.
#[derive(Debug, Serialize)]
pub struct EmailSearchOutput {
    pub criteria: String,
    pub result_count: usize,
    pub results: Vec<EmailSearchResult>,
}

impl Tool for EmailSearchTool {
    const NAME: &'static str = "email_search";

    type Error = EmailSearchError;
    type Args = EmailSearchArgs;
    type Output = EmailSearchOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: crate::prompts::text::get("tools/email_search").to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Optional full-text query for message bodies and headers."
                    },
                    "from": {
                        "type": "string",
                        "description": "Optional sender filter (e.g. alice@example.com)."
                    },
                    "subject": {
                        "type": "string",
                        "description": "Optional subject filter."
                    },
                    "folders": {
                        "type": "array",
                        "description": "Optional folder names to search. Defaults to configured folders.",
                        "items": { "type": "string" }
                    },
                    "unread_only": {
                        "type": "boolean",
                        "description": "When true, search only unread messages."
                    },
                    "since_days": {
                        "type": "integer",
                        "description": "Search lookback in days (defaults to 30)."
                    },
                    "limit": {
                        "type": "integer",
                        "description": "Maximum results (1-50, default 10)."
                    }
                }
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let query = EmailSearchQuery {
            text: clean_optional(args.query),
            from: clean_optional(args.from),
            subject: clean_optional(args.subject),
            unread_only: args.unread_only,
            since_days: args.since_days.filter(|days| *days > 0).or(Some(30)),
            folders: args.folders,
            limit: args.limit.unwrap_or(10).clamp(1, 50),
        };

        let criteria = format_search_criteria(&query);

        let instance_dir = self.runtime_config.instance_dir.clone();
        let search_query = query.clone();
        let hits = tokio::task::spawn_blocking(move || {
            let email_config = load_email_config(&instance_dir)?;
            crate::messaging::email::search_mailbox(&email_config, search_query)
                .map_err(|error| EmailSearchError(error.to_string()))
        })
        .await
        .map_err(|error| EmailSearchError(format!("email search task failed: {error}")))??;

        let results = hits
            .into_iter()
            .map(|hit| EmailSearchResult {
                folder: hit.folder,
                uid: hit.uid,
                from: hit.from,
                subject: hit.subject,
                date: hit.date,
                message_id: hit.message_id,
                body_snippet: truncate_snippet(&hit.body, 1600),
                attachment_names: hit.attachment_names,
            })
            .collect::<Vec<_>>();

        Ok(EmailSearchOutput {
            criteria,
            result_count: results.len(),
            results,
        })
    }
}

fn load_email_config(instance_dir: &Path) -> Result<EmailConfig, EmailSearchError> {
    let config = Config::load_for_instance(instance_dir).map_err(|error| {
        EmailSearchError(format!(
            "failed to resolve config for {}: {error}",
            instance_dir.display()
        ))
    })?;

    let email = config
        .messaging
        .email
        .ok_or_else(|| EmailSearchError("email adapter is not configured".to_string()))?;

    if !email.enabled {
        return Err(EmailSearchError(
            "email adapter is configured but disabled".to_string(),
        ));
    }

    Ok(email)
}

fn clean_optional(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn format_search_criteria(query: &EmailSearchQuery) -> String {
    let mut parts = Vec::new();
    if let Some(from) = &query.from {
        parts.push(format!("from={from}"));
    }
    if let Some(subject) = &query.subject {
        parts.push(format!("subject={subject}"));
    }
    if let Some(text) = &query.text {
        parts.push(format!("query={text}"));
    }
    if query.unread_only {
        parts.push("unread_only=true".to_string());
    }
    if let Some(since_days) = query.since_days {
        parts.push(format!("since_days={since_days}"));
    }
    if !query.folders.is_empty() {
        parts.push(format!("folders={}", query.folders.join(",")));
    }
    parts.push(format!("limit={}", query.limit));

    parts.join("; ")
}

fn truncate_snippet(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_string();
    }

    let mut end = max_bytes;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }

    format!("{}\n\n[snippet truncated]", &value[..end])
}

#[cfg(test)]
mod tests {
    use super::{clean_optional, format_search_criteria};
    use crate::messaging::email::EmailSearchQuery;

    #[test]
    fn clean_optional_rejects_empty_values() {
        assert_eq!(clean_optional(None), None);
        assert_eq!(clean_optional(Some("   ".to_string())), None);
        assert_eq!(
            clean_optional(Some("  value ".to_string())),
            Some("value".to_string())
        );
    }

    #[test]
    fn format_search_criteria_includes_defaults() {
        let criteria = format_search_criteria(&EmailSearchQuery {
            text: None,
            from: None,
            subject: None,
            unread_only: false,
            since_days: Some(30),
            folders: Vec::new(),
            limit: 10,
        });

        assert_eq!(criteria, "since_days=30; limit=10");
    }
}
