//! MCP tool adapters that proxy calls to external MCP servers.

use crate::mcp::McpConnection;
use crate::tools::truncate_output;

use rig::completion::ToolDefinition;
use rig::tool::Tool;
use serde::Serialize;
use serde_json::Value;
use std::sync::Arc;

#[derive(Clone)]
pub struct McpToolAdapter {
    server_name: String,
    tool_name: String,
    description: String,
    input_schema: Value,
    connection: Arc<McpConnection>,
}

impl McpToolAdapter {
    pub fn new(
        server_name: String,
        tool: rmcp::model::Tool,
        connection: Arc<McpConnection>,
    ) -> Self {
        let input_schema = tool.schema_as_json_value();
        let description = tool
            .description
            .map(|description| description.into_owned())
            .unwrap_or_default();

        Self {
            server_name,
            tool_name: tool.name.into_owned(),
            description,
            input_schema,
            connection,
        }
    }

    fn namespaced_name(&self) -> String {
        format!(
            "{}_{}",
            sanitize_tool_identifier(&self.server_name),
            sanitize_tool_identifier(&self.tool_name)
        )
    }

    fn collect_result_text(result: &rmcp::model::CallToolResult) -> String {
        let blocks = result
            .content
            .iter()
            .map(|content| match &content.raw {
                rmcp::model::RawContent::Text(text) => text.text.clone(),
                rmcp::model::RawContent::Resource(resource) => match &resource.resource {
                    rmcp::model::ResourceContents::TextResourceContents { text, .. } => {
                        text.clone()
                    }
                    _ => serde_json::to_string(&content.raw)
                        .unwrap_or_else(|_| "[unsupported resource content]".to_string()),
                },
                other => serde_json::to_string(other)
                    .unwrap_or_else(|_| "[unsupported mcp content]".to_string()),
            })
            .collect::<Vec<_>>();

        if blocks.is_empty() {
            result
                .structured_content
                .as_ref()
                .map(Value::to_string)
                .unwrap_or_default()
        } else {
            blocks.join("\n")
        }
    }

    fn bounded_structured_content(result: &rmcp::model::CallToolResult) -> Option<Value> {
        let structured_content = result.structured_content.as_ref()?;
        let serialized = structured_content.to_string();
        if serialized.len() > crate::tools::MAX_TOOL_OUTPUT_BYTES {
            return None;
        }

        Some(structured_content.clone())
    }

    fn format_output(
        server_name: &str,
        tool_name: &str,
        result: &rmcp::model::CallToolResult,
    ) -> McpToolOutput {
        let output_text = Self::collect_result_text(result);
        let output_text = truncate_output(&output_text, crate::tools::MAX_TOOL_OUTPUT_BYTES);
        let is_error = result.is_error.unwrap_or(false);
        let structured_content = Self::bounded_structured_content(result);

        let result_text = if output_text.is_empty() {
            if is_error {
                format!("MCP server '{server_name}' reported an error while calling '{tool_name}'")
            } else {
                "[tool returned no content]".to_string()
            }
        } else {
            output_text
        };

        McpToolOutput {
            server: server_name.to_string(),
            tool: tool_name.to_string(),
            result: result_text,
            structured_content,
            is_error,
        }
    }
}

#[derive(Debug, thiserror::Error)]
#[error("MCP tool call failed: {0}")]
pub struct McpToolError(String);

#[derive(Debug, Serialize)]
pub struct McpToolOutput {
    pub server: String,
    pub tool: String,
    pub result: String,
    pub structured_content: Option<Value>,
    pub is_error: bool,
}

impl Tool for McpToolAdapter {
    const NAME: &'static str = "mcp_tool";

    type Error = McpToolError;
    type Args = Value;
    type Output = McpToolOutput;

    fn name(&self) -> String {
        self.namespaced_name()
    }

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: self.namespaced_name(),
            description: self.description.clone(),
            parameters: self.input_schema.clone(),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let result = self
            .connection
            .call_tool(&self.tool_name, args)
            .await
            .map_err(|error| {
                McpToolError(format!(
                    "server '{}' tool '{}' failed: {}",
                    self.server_name, self.tool_name, error
                ))
            })?;

        Ok(Self::format_output(
            &self.server_name,
            &self.tool_name,
            &result,
        ))
    }
}

fn sanitize_tool_identifier(raw: &str) -> String {
    let mut value = raw
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect::<String>();

    while value.contains("__") {
        value = value.replace("__", "_");
    }

    value = value.trim_matches('_').to_string().if_empty("mcp_tool");

    if value
        .chars()
        .next()
        .is_some_and(|character| character.is_ascii_digit())
    {
        value.insert(0, '_');
    }

    value
}

trait EmptyStringExt {
    fn if_empty(self, fallback: &str) -> String;
}

impl EmptyStringExt for String {
    fn if_empty(self, fallback: &str) -> String {
        if self.is_empty() {
            fallback.to_string()
        } else {
            self
        }
    }
}

#[cfg(test)]
mod tests {
    use super::McpToolAdapter;
    use rmcp::model::{CallToolResult, Content};
    use serde_json::json;

    #[test]
    fn collect_result_text_uses_structured_content_only_once_when_content_is_empty() {
        let mut result = CallToolResult::structured(json!({ "status": "ok" }));
        result.content = vec![];

        let text = McpToolAdapter::collect_result_text(&result);
        assert_eq!(text, "{\"status\":\"ok\"}");
    }

    #[test]
    fn format_output_keeps_mcp_declared_errors_in_structured_result() {
        let result = CallToolResult::error(vec![Content::text("remote failed")]);

        let output = McpToolAdapter::format_output("demo", "search", &result);
        assert!(output.is_error);
        assert_eq!(output.result, "remote failed");
    }

    #[test]
    fn format_output_drops_oversized_structured_content() {
        let mut result = CallToolResult::structured(json!({
            "payload": "x".repeat(crate::tools::MAX_TOOL_OUTPUT_BYTES)
        }));
        result.content = vec![Content::text("small")];

        let output = McpToolAdapter::format_output("demo", "search", &result);
        assert!(output.structured_content.is_none());
        assert!(!output.is_error);
        assert_eq!(output.result, "small");
    }
}
