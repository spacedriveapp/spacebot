//! Read embedded Spacebot docs, AGENTS guide, and changelog notes.

use rig::completion::ToolDefinition;
use rig::tool::Tool;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Tool for reading Spacebot's embedded self-documentation.
#[derive(Debug, Clone)]
pub struct SpacebotDocsTool;

impl SpacebotDocsTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for SpacebotDocsTool {
    fn default() -> Self {
        Self::new()
    }
}

/// Error type for `spacebot_docs`.
#[derive(Debug, thiserror::Error)]
#[error("spacebot_docs failed: {0}")]
pub struct SpacebotDocsError(String);

/// Arguments for `spacebot_docs`.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct SpacebotDocsArgs {
    /// Action to perform: `list` or `read`.
    #[serde(default = "default_action")]
    pub action: String,
    /// Document ID/path to read (required when `action = "read"`).
    pub doc_id: Option<String>,
    /// Optional text filter for list output (or fallback lookup hint for read).
    pub query: Option<String>,
    /// 1-based line number to start reading from.
    #[serde(default = "default_start_line")]
    pub start_line: usize,
    /// Maximum lines to return.
    #[serde(default = "default_max_lines")]
    pub max_lines: usize,
}

fn default_action() -> String {
    "list".to_string()
}

fn default_start_line() -> usize {
    1
}

fn default_max_lines() -> usize {
    300
}

/// Read payload for `action = "read"`.
#[derive(Debug, Serialize)]
pub struct SpacebotDocContent {
    pub id: String,
    pub title: String,
    pub path: String,
    pub section: String,
    pub start_line: usize,
    pub end_line: usize,
    pub total_lines: usize,
    pub has_more: bool,
    pub content: String,
}

/// Output from `spacebot_docs`.
#[derive(Debug, Serialize)]
pub struct SpacebotDocsOutput {
    pub success: bool,
    pub action: String,
    pub message: String,
    pub error: Option<String>,
    pub docs: Vec<crate::self_awareness::EmbeddedDocSummary>,
    pub document: Option<SpacebotDocContent>,
}

impl Tool for SpacebotDocsTool {
    const NAME: &'static str = "spacebot_docs";

    type Error = SpacebotDocsError;
    type Args = SpacebotDocsArgs;
    type Output = SpacebotDocsOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: crate::prompts::text::get("tools/spacebot_docs").to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["list", "read"],
                        "default": "list",
                        "description": "Use `list` to discover available docs, or `read` to fetch one doc by ID/path."
                    },
                    "doc_id": {
                        "type": "string",
                        "description": "Doc ID or path to read (required for `read`). Example IDs: `agents`, `changelog`, `docs/core/cortex`."
                    },
                    "query": {
                        "type": "string",
                        "description": "Optional filter for list output (matches ID/title/path/section)."
                    },
                    "start_line": {
                        "type": "integer",
                        "minimum": 1,
                        "default": 1,
                        "description": "1-based line number to start from when reading."
                    },
                    "max_lines": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": 2000,
                        "default": 300,
                        "description": "Maximum number of lines to return when reading."
                    }
                }
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let action = args.action.trim().to_ascii_lowercase();

        match action.as_str() {
            "list" => {
                let mut docs = crate::self_awareness::list_embedded_docs(args.query.as_deref());
                if docs.len() > 200 {
                    docs.truncate(200);
                }

                Ok(SpacebotDocsOutput {
                    success: true,
                    action,
                    message: format!(
                        "{} docs available{}",
                        docs.len(),
                        args.query
                            .as_deref()
                            .filter(|query| !query.trim().is_empty())
                            .map(|query| format!(" (filtered by '{query}')"))
                            .unwrap_or_default()
                    ),
                    error: None,
                    docs,
                    document: None,
                })
            }
            "read" => {
                let requested_id = args
                    .doc_id
                    .as_deref()
                    .or(args.query.as_deref())
                    .map(str::trim)
                    .filter(|value| !value.is_empty());

                let Some(requested_id) = requested_id else {
                    let error =
                        "`doc_id` is required for action=read. Call with action=list first if you need IDs.".to_string();
                    return Ok(failure_output(&action, error));
                };

                let Some(document) = crate::self_awareness::get_embedded_doc(requested_id) else {
                    return Ok(failure_output(&action, unknown_doc_message(requested_id)));
                };

                let requested_start_line = args.start_line.max(1);
                let max_lines = args.max_lines.clamp(1, 2000);
                let (content, end_line, total_lines, has_more) =
                    slice_lines(&document.content, requested_start_line, max_lines);
                let start_line = normalize_start_line(requested_start_line, total_lines);
                let message = if total_lines == 0 {
                    format!("read '{}' empty document", document.summary.id)
                } else if requested_start_line > total_lines {
                    format!(
                        "read '{}' start_line {} past EOF; normalized to {} of {}",
                        document.summary.id, requested_start_line, total_lines, total_lines
                    )
                } else {
                    format!(
                        "read '{}' lines {}-{} of {}",
                        document.summary.id, start_line, end_line, total_lines
                    )
                };

                Ok(SpacebotDocsOutput {
                    success: true,
                    action,
                    message,
                    error: None,
                    docs: Vec::new(),
                    document: Some(SpacebotDocContent {
                        id: document.summary.id,
                        title: document.summary.title,
                        path: document.summary.path,
                        section: document.summary.section,
                        start_line,
                        end_line,
                        total_lines,
                        has_more,
                        content,
                    }),
                })
            }
            other => Ok(failure_output(
                &action,
                format!("invalid action '{other}'. valid actions: list, read"),
            )),
        }
    }
}

fn failure_output(action: &str, error: String) -> SpacebotDocsOutput {
    SpacebotDocsOutput {
        success: false,
        action: action.to_string(),
        message: error.clone(),
        error: Some(error),
        docs: Vec::new(),
        document: None,
    }
}

fn unknown_doc_message(requested_id: &str) -> String {
    let matches = crate::self_awareness::search_embedded_docs(requested_id);
    if matches.is_empty() {
        return format!(
            "document '{requested_id}' not found. Use action=list to see available IDs."
        );
    }

    let suggestions = matches
        .iter()
        .take(8)
        .map(|doc| doc.id.as_str())
        .collect::<Vec<_>>()
        .join(", ");

    format!(
        "document '{requested_id}' is ambiguous or not an exact match. Try one of: {suggestions}"
    )
}

fn slice_lines(content: &str, start_line: usize, max_lines: usize) -> (String, usize, usize, bool) {
    let total_lines = content.lines().count();

    if total_lines == 0 {
        return (String::new(), 0, 0, false);
    }

    if start_line > total_lines {
        return (String::new(), total_lines, total_lines, false);
    }

    let start_index = start_line.saturating_sub(1);
    let end_index = (start_index + max_lines).min(total_lines);
    let has_more = end_index < total_lines;
    let content = content
        .lines()
        .skip(start_index)
        .take(end_index - start_index)
        .collect::<Vec<_>>()
        .join("\n");

    (content, end_index, total_lines, has_more)
}

fn normalize_start_line(requested_start_line: usize, total_lines: usize) -> usize {
    if total_lines == 0 {
        0
    } else {
        requested_start_line.min(total_lines)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_start_line_caps_to_total_lines() {
        assert_eq!(normalize_start_line(20, 5), 5);
    }

    #[test]
    fn normalize_start_line_for_empty_doc_is_zero() {
        assert_eq!(normalize_start_line(1, 0), 0);
    }

    #[test]
    fn slice_lines_past_eof_returns_empty_at_eof() {
        let (content, end_line, total_lines, has_more) = slice_lines("a\nb\nc", 10, 100);
        assert!(content.is_empty());
        assert_eq!(end_line, 3);
        assert_eq!(total_lines, 3);
        assert!(!has_more);
    }

    #[tokio::test]
    async fn read_without_doc_id_returns_structured_failure() {
        let output = SpacebotDocsTool::new()
            .call(SpacebotDocsArgs {
                action: "read".to_string(),
                doc_id: None,
                query: None,
                start_line: 1,
                max_lines: 100,
            })
            .await
            .expect("tool call should not error");

        assert!(!output.success);
        assert!(output.error.is_some());
        assert!(output.document.is_none());
    }

    #[tokio::test]
    async fn invalid_action_returns_structured_failure() {
        let output = SpacebotDocsTool::new()
            .call(SpacebotDocsArgs {
                action: "nope".to_string(),
                doc_id: None,
                query: None,
                start_line: 1,
                max_lines: 100,
            })
            .await
            .expect("tool call should not error");

        assert!(!output.success);
        assert!(output.error.is_some());
        assert!(output.error.unwrap().contains("invalid action"));
    }

    #[tokio::test]
    async fn unknown_doc_returns_structured_failure() {
        let output = SpacebotDocsTool::new()
            .call(SpacebotDocsArgs {
                action: "read".to_string(),
                doc_id: Some("definitely-not-a-real-doc-id".to_string()),
                query: None,
                start_line: 1,
                max_lines: 100,
            })
            .await
            .expect("tool call should not error");

        assert!(!output.success);
        assert!(output.error.is_some());
        assert!(output.document.is_none());
    }
}
