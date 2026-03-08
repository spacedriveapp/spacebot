//! QMD adapter for the native knowledge retrieval plane.

use crate::knowledge::registry::QMD_SOURCE_ID;
use crate::knowledge::types::{
    KnowledgeHit, KnowledgeProvenance, KnowledgeQuery, KnowledgeSourceKind,
};
use crate::mcp::McpManager;

use serde_json::Value;

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use super::service::KnowledgeServiceError;

const QMD_SERVER_NAME: &str = "qmd";
const QMD_QUERY_TOOL_NAME: &str = "query";
const QMD_SOURCE_LABEL: &str = "QMD";
const QMD_RETRIEVAL_TIMEOUT: Duration = Duration::from_secs(5);

type QmdToolFuture =
    Pin<Box<dyn Future<Output = Result<QmdToolResponse, KnowledgeServiceError>> + Send>>;
type QmdToolCaller = dyn Fn(Value) -> QmdToolFuture + Send + Sync;

/// Normalized subset of the QMD query response the retrieval plane consumes.
#[derive(Debug, Clone, PartialEq)]
pub struct QmdToolResponse {
    pub result_text: String,
    pub structured_content: Option<Value>,
}

/// Normalized retrieval outcome from QMD, including malformed-row loss.
#[derive(Debug, Clone, PartialEq)]
pub struct QmdRetrievalOutcome {
    pub hits: Vec<KnowledgeHit>,
    pub malformed_result_count: usize,
}

/// QMD retrieval adapter backed by the MCP manager.
#[derive(Clone)]
pub struct QmdAdapter {
    tool_caller: Arc<QmdToolCaller>,
}

impl std::fmt::Debug for QmdAdapter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("QmdAdapter").finish_non_exhaustive()
    }
}

impl QmdAdapter {
    pub fn new(mcp_manager: Arc<McpManager>) -> Self {
        let tool_caller = Arc::new(move |arguments: Value| {
            let mcp_manager = mcp_manager.clone();
            Box::pin(async move {
                let result = tokio::time::timeout(
                    QMD_RETRIEVAL_TIMEOUT,
                    mcp_manager.call_tool(QMD_SERVER_NAME, QMD_QUERY_TOOL_NAME, arguments),
                )
                .await
                .map_err(|_| {
                    KnowledgeServiceError::Message(
                        "QMD MCP server timed out during retrieval.".to_string(),
                    )
                })?
                .map_err(|error| {
                    KnowledgeServiceError::Message(normalize_qmd_call_error(&error.to_string()))
                })?;

                Ok(QmdToolResponse {
                    result_text: collect_result_text(&result),
                    structured_content: result.structured_content.clone(),
                })
            }) as QmdToolFuture
        });

        Self { tool_caller }
    }

    pub async fn retrieve(
        &self,
        query: &KnowledgeQuery,
    ) -> Result<QmdRetrievalOutcome, KnowledgeServiceError> {
        let response = (self.tool_caller)(build_query_payload(query)).await?;
        normalize_qmd_hits(&response, query.max_results)
    }

    #[cfg(test)]
    pub fn from_static_result(result: Result<QmdToolResponse, KnowledgeServiceError>) -> Self {
        let (ok_result, error_message) = match result {
            Ok(response) => (Some(response), None),
            Err(KnowledgeServiceError::Message(message)) => (None, Some(message)),
        };
        let tool_caller = Arc::new(move |_arguments: Value| {
            let ok_result = ok_result.clone();
            let error_message = error_message.clone();
            Box::pin(async move {
                match (ok_result, error_message) {
                    (Some(response), None) => Ok(response),
                    (None, Some(message)) => Err(KnowledgeServiceError::Message(message)),
                    _ => Err(KnowledgeServiceError::Message(
                        "QMD fixture adapter is misconfigured.".to_string(),
                    )),
                }
            }) as QmdToolFuture
        });

        Self { tool_caller }
    }
}

fn build_query_payload(query: &KnowledgeQuery) -> Value {
    serde_json::json!({
        "searches": [
            {
                "type": "lex",
                "query": query.query,
            }
        ],
        "limit": query.max_results,
    })
}

fn normalize_qmd_hits(
    response: &QmdToolResponse,
    max_results: usize,
) -> Result<QmdRetrievalOutcome, KnowledgeServiceError> {
    let structured_content = response.structured_content.as_ref().ok_or_else(|| {
        KnowledgeServiceError::Message("QMD returned no structured search results.".to_string())
    })?;
    let results = structured_content
        .get("results")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            KnowledgeServiceError::Message(
                "QMD returned an unexpected structured result shape.".to_string(),
            )
        })?;

    let mut hits = Vec::new();
    let mut malformed_result_count = 0usize;

    for value in results {
        match normalize_qmd_hit(value) {
            Ok(hit) => {
                hits.push(hit);
                if hits.len() == max_results {
                    break;
                }
            }
            Err(error) => {
                malformed_result_count += 1;
                tracing::warn!(%error, "dropping malformed qmd retrieval result");
            }
        }
    }

    if hits.is_empty() && malformed_result_count > 0 {
        return Err(KnowledgeServiceError::Message(format!(
            "QMD returned no usable search results; dropped {malformed_result_count} malformed result(s)."
        )));
    }

    Ok(QmdRetrievalOutcome {
        hits,
        malformed_result_count,
    })
}

fn normalize_qmd_hit(value: &Value) -> Result<KnowledgeHit, KnowledgeServiceError> {
    let docid = required_string_field(value, "docid")?;
    let file = required_string_field(value, "file")?;
    let title = required_string_field(value, "title")?;
    let score = value.get("score").and_then(Value::as_f64).ok_or_else(|| {
        KnowledgeServiceError::Message("QMD result omitted a numeric score.".to_string())
    })? as f32;
    let snippet = required_string_field(value, "snippet")?;
    let context = value
        .get("context")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|context| !context.is_empty());

    let snippet = if let Some(context) = context {
        format!("{context}\n\n{snippet}")
    } else {
        snippet.to_string()
    };

    Ok(KnowledgeHit {
        id: format!("{QMD_SOURCE_ID}:{docid}"),
        title: title.to_string(),
        snippet,
        content_type: "text/markdown".to_string(),
        score,
        provenance: KnowledgeProvenance {
            source_id: QMD_SOURCE_ID.to_string(),
            source_kind: KnowledgeSourceKind::Qmd,
            source_label: QMD_SOURCE_LABEL.to_string(),
            canonical_locator: canonical_qmd_locator(file),
        },
    })
}

fn required_string_field<'a>(
    value: &'a Value,
    field_name: &str,
) -> Result<&'a str, KnowledgeServiceError> {
    value
        .get(field_name)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|field| !field.is_empty())
        .ok_or_else(|| {
            KnowledgeServiceError::Message(format!(
                "QMD result omitted the required '{field_name}' field."
            ))
        })
}

fn canonical_qmd_locator(file: &str) -> String {
    let encoded = file
        .split('/')
        .map(|segment| urlencoding::encode(segment).into_owned())
        .collect::<Vec<_>>()
        .join("/");

    format!("qmd://{encoded}")
}

fn normalize_qmd_call_error(message: &str) -> String {
    let lower = message.to_ascii_lowercase();
    if lower.contains("not connected") {
        return "QMD MCP server is configured but not connected.".to_string();
    }
    if lower.contains("timed out") {
        return "QMD MCP server timed out during retrieval.".to_string();
    }
    if lower.contains("connection refused") || lower.contains("failed to initialize") {
        return "QMD MCP server is unavailable right now.".to_string();
    }

    "QMD retrieval failed before normalized results were available.".to_string()
}

fn collect_result_text(result: &rmcp::model::CallToolResult) -> String {
    let mut blocks = result
        .content
        .iter()
        .map(|content| match &content.raw {
            rmcp::model::RawContent::Text(text) => text.text.clone(),
            rmcp::model::RawContent::Resource(resource) => match &resource.resource {
                rmcp::model::ResourceContents::TextResourceContents { text, .. } => text.clone(),
                _ => serde_json::to_string(&content.raw)
                    .unwrap_or_else(|_| "[unsupported resource content]".to_string()),
            },
            other => serde_json::to_string(other)
                .unwrap_or_else(|_| "[unsupported mcp content]".to_string()),
        })
        .collect::<Vec<_>>();

    if let Some(structured_content) = &result.structured_content {
        blocks.push(structured_content.to_string());
    }

    blocks.join("\n")
}

#[cfg(test)]
mod tests {
    use super::{
        QmdToolResponse, canonical_qmd_locator, normalize_qmd_call_error, normalize_qmd_hits,
    };
    use crate::knowledge::types::KnowledgeSourceKind;

    #[test]
    fn normalize_qmd_hits_preserves_provenance() {
        let outcome = normalize_qmd_hits(
            &QmdToolResponse {
                result_text: "Found 1 result".to_string(),
                structured_content: Some(serde_json::json!({
                    "results": [
                        {
                            "docid": "#abc123",
                            "file": "vault/notes/knowledge plane.md",
                            "title": "Knowledge Plane",
                            "score": 0.87,
                            "context": "Architecture notes",
                            "snippet": "The retrieval plane keeps native memory authoritative."
                        }
                    ]
                })),
            },
            10,
        )
        .expect("normalize qmd hits");

        assert_eq!(outcome.malformed_result_count, 0);
        assert_eq!(outcome.hits.len(), 1);
        assert_eq!(outcome.hits[0].provenance.source_id, "qmd");
        assert_eq!(
            outcome.hits[0].provenance.source_kind,
            KnowledgeSourceKind::Qmd
        );
        assert_eq!(
            outcome.hits[0].provenance.canonical_locator,
            "qmd://vault/notes/knowledge%20plane.md"
        );
        assert!(outcome.hits[0].snippet.contains("Architecture notes"));
    }

    #[test]
    fn normalize_qmd_hits_rejects_missing_results() {
        let error = normalize_qmd_hits(
            &QmdToolResponse {
                result_text: "No content".to_string(),
                structured_content: Some(serde_json::json!({ "items": [] })),
            },
            10,
        )
        .expect_err("missing results should fail");

        assert!(
            error
                .to_string()
                .contains("unexpected structured result shape")
        );
    }

    #[test]
    fn normalize_qmd_call_error_hides_transport_details() {
        assert_eq!(
            normalize_qmd_call_error("mcp server 'qmd' is not connected"),
            "QMD MCP server is configured but not connected."
        );
        assert_eq!(
            normalize_qmd_call_error("failed to initialize mcp server 'qmd'"),
            "QMD MCP server is unavailable right now."
        );
    }

    #[test]
    fn canonical_qmd_locator_encodes_segments() {
        assert_eq!(
            canonical_qmd_locator("notes/with spaces.md"),
            "qmd://notes/with%20spaces.md"
        );
    }

    #[test]
    fn normalize_qmd_hits_keeps_valid_rows_when_one_result_is_malformed() {
        let outcome = normalize_qmd_hits(
            &QmdToolResponse {
                result_text: "Found 2 results".to_string(),
                structured_content: Some(serde_json::json!({
                    "results": [
                        {
                            "docid": "#abc123",
                            "file": "vault/notes/knowledge plane.md",
                            "title": "Knowledge Plane",
                            "score": 0.87,
                            "context": "Architecture notes",
                            "snippet": "The retrieval plane keeps native memory authoritative."
                        },
                        {
                            "docid": "#broken",
                            "file": "",
                            "title": "Broken",
                            "score": 0.52,
                            "snippet": "missing file should be dropped"
                        }
                    ]
                })),
            },
            10,
        )
        .expect("normalize qmd hits");

        assert_eq!(outcome.malformed_result_count, 1);
        assert_eq!(outcome.hits.len(), 1);
        assert_eq!(outcome.hits[0].provenance.source_id, "qmd");
    }

    #[test]
    fn normalize_qmd_hits_keeps_searching_after_malformed_leading_rows() {
        let outcome = normalize_qmd_hits(
            &QmdToolResponse {
                result_text: "Found 3 results".to_string(),
                structured_content: Some(serde_json::json!({
                    "results": [
                        {
                            "docid": "#broken-1",
                            "file": "",
                            "title": "Broken 1",
                            "score": 0.52,
                            "snippet": "missing file should be dropped"
                        },
                        {
                            "docid": "#broken-2",
                            "file": "",
                            "title": "Broken 2",
                            "score": 0.51,
                            "snippet": "missing file should be dropped"
                        },
                        {
                            "docid": "#abc123",
                            "file": "vault/notes/knowledge plane.md",
                            "title": "Knowledge Plane",
                            "score": 0.87,
                            "context": "Architecture notes",
                            "snippet": "The retrieval plane keeps native memory authoritative."
                        }
                    ]
                })),
            },
            1,
        )
        .expect("normalize qmd hits");

        assert_eq!(outcome.malformed_result_count, 2);
        assert_eq!(outcome.hits.len(), 1);
        assert_eq!(outcome.hits[0].title, "Knowledge Plane");
    }
}
