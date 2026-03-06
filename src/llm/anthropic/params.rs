//! Build full Anthropic API requests with auth, system prompt, thinking, and tools.

use super::auth::{self, AnthropicAuthPath};
use super::cache;
use super::tools;

use reqwest::RequestBuilder;
use rig::completion::{CompletionError, CompletionRequest};
use rig::message::ToolChoice;

const CLAUDE_CODE_SYSTEM_PREAMBLE: &str =
    "You are Claude Code, Anthropic's official CLI for Claude.";

/// Result of building an Anthropic request: the configured HTTP request builder,
/// the auth path used, and the original tool names for response reverse-mapping.
pub struct AnthropicRequest {
    pub builder: RequestBuilder,
    pub auth_path: AnthropicAuthPath,
    /// Original tool (name, description) pairs for reverse-mapping response tool calls.
    pub original_tools: Vec<(String, String)>,
}

/// Request-time behavior toggles for Anthropic request assembly.
pub struct AnthropicRequestOptions {
    pub force_bearer: bool,
    pub apply_tool_choice: bool,
    pub apply_output_schema: bool,
}

/// Adaptive thinking is only available on 4.6-generation models.
fn supports_adaptive_thinking(model_id: &str) -> bool {
    model_id.contains("opus-4-6")
        || model_id.contains("opus-4.6")
        || model_id.contains("sonnet-4-6")
        || model_id.contains("sonnet-4.6")
}

fn is_opus(model_id: &str) -> bool {
    model_id.contains("opus")
}

/// Construct the full messages endpoint URL from a base URL.
///
/// If the base URL already ends with a path segment (e.g. `/v1/messages`),
/// use it as-is. Otherwise append `/v1/messages`.
fn messages_url(base_url: &str) -> String {
    let trimmed = base_url.trim_end_matches('/');
    if trimmed.ends_with("/v1/messages") {
        trimmed.to_string()
    } else {
        format!("{trimmed}/v1/messages")
    }
}

/// Build a fully configured Anthropic API request from a CompletionRequest.
///
/// `base_url` is the provider's configured base URL (e.g. `https://api.anthropic.com`
/// or a custom proxy). The `/v1/messages` path is appended automatically.
///
/// `thinking_effort` controls adaptive thinking: "auto" picks max for Opus /
/// high for others, or pass "max", "high", "medium", "low" explicitly.
pub fn build_anthropic_request(
    http_client: &reqwest::Client,
    api_key: &str,
    base_url: &str,
    model_name: &str,
    request: &CompletionRequest,
    thinking_effort: &str,
    options: AnthropicRequestOptions,
) -> Result<AnthropicRequest, CompletionError> {
    let AnthropicRequestOptions {
        force_bearer,
        apply_tool_choice,
        apply_output_schema,
    } = options;
    let is_oauth = auth::detect_auth_path(api_key, force_bearer) == AnthropicAuthPath::OAuthToken;
    let adaptive_thinking = supports_adaptive_thinking(model_name);
    let retention = cache::resolve_cache_retention(None);
    let url = messages_url(base_url);
    let cache_control = cache::get_cache_control(&url, retention);

    let mut body = serde_json::json!({
        "model": model_name,
        "max_tokens": request.max_tokens.unwrap_or(16_000),
    });

    build_system_prompt(&mut body, request, is_oauth, &cache_control);

    let messages = crate::llm::model::convert_messages_to_anthropic(&request.chat_history);
    body["messages"] = serde_json::json!(messages);

    let original_tools = build_tools(&mut body, request, is_oauth, &cache_control);
    if apply_tool_choice && let Some(tool_choice) = request.tool_choice.as_ref() {
        body["tool_choice"] = map_anthropic_tool_choice(tool_choice)?;
    }
    if apply_output_schema && let Some(output_schema) = request.output_schema.as_ref() {
        let mut schema = output_schema.clone().to_value();
        sanitize_anthropic_schema(&mut schema);
        body["output_config"] = serde_json::json!({
            "format": {
                "type": "json_schema",
                "schema": schema,
            }
        });
    }

    if let Some(temperature) = request.temperature {
        body["temperature"] = serde_json::json!(temperature);
    }

    if adaptive_thinking {
        body["thinking"] = serde_json::json!({ "type": "adaptive" });
        let effort = match thinking_effort {
            "max" | "high" | "medium" | "low" => thinking_effort,
            _ => {
                if is_opus(model_name) {
                    "max"
                } else {
                    "high"
                }
            }
        };
        if body["output_config"].is_object() {
            body["output_config"]["effort"] = serde_json::json!(effort);
        } else {
            body["output_config"] = serde_json::json!({ "effort": effort });
        }
    }

    let builder = http_client
        .post(&url)
        .header("anthropic-version", "2023-06-01")
        .header("content-type", "application/json");

    let (builder, auth_path) = auth::apply_auth_headers(builder, api_key, false, force_bearer);
    let builder = builder.json(&body);

    Ok(AnthropicRequest {
        builder,
        auth_path,
        original_tools,
    })
}

fn build_system_prompt(
    body: &mut serde_json::Value,
    request: &CompletionRequest,
    is_oauth: bool,
    cache_control: &Option<serde_json::Value>,
) {
    let mut system_blocks: Vec<serde_json::Value> = Vec::new();

    if is_oauth {
        let mut identity_block = serde_json::json!({
            "type": "text",
            "text": CLAUDE_CODE_SYSTEM_PREAMBLE,
        });
        if let Some(cc) = cache_control {
            identity_block["cache_control"] = cc.clone();
        }
        system_blocks.push(identity_block);
    }

    if let Some(preamble) = &request.preamble {
        let mut preamble_block = serde_json::json!({
            "type": "text",
            "text": preamble,
        });
        if let Some(cc) = cache_control {
            preamble_block["cache_control"] = cc.clone();
        }
        system_blocks.push(preamble_block);
    }

    if !system_blocks.is_empty() {
        body["system"] = serde_json::json!(system_blocks);
    }
}

fn map_anthropic_tool_choice(
    tool_choice: &ToolChoice,
) -> Result<serde_json::Value, CompletionError> {
    let mapped = match tool_choice {
        ToolChoice::Auto => serde_json::json!({ "type": "auto" }),
        ToolChoice::None => serde_json::json!({ "type": "none" }),
        ToolChoice::Required => serde_json::json!({ "type": "any" }),
        ToolChoice::Specific { function_names } => {
            let Some(function_name) = function_names.first() else {
                return Err(CompletionError::ProviderError(
                    "tool_choice specific requires at least one function name".to_string(),
                ));
            };
            if function_names.len() > 1 {
                return Err(CompletionError::ProviderError(
                    "Anthropic only supports one specific tool name".to_string(),
                ));
            }
            serde_json::json!({
                "type": "tool",
                "name": function_name,
            })
        }
    };
    Ok(mapped)
}

fn sanitize_anthropic_schema(schema: &mut serde_json::Value) {
    use serde_json::Value;

    if let Value::Object(object) = schema {
        let is_object_schema = object.get("type") == Some(&Value::String("object".to_string()))
            || object.contains_key("properties");

        if is_object_schema && !object.contains_key("additionalProperties") {
            object.insert("additionalProperties".to_string(), Value::Bool(false));
        }

        for key in ["$defs", "properties"] {
            if let Some(Value::Object(property_map)) = object.get_mut(key) {
                for property in property_map.values_mut() {
                    sanitize_anthropic_schema(property);
                }
            }
        }

        if let Some(items) = object.get_mut("items") {
            sanitize_anthropic_schema(items);
        }

        for key in ["anyOf", "allOf", "oneOf"] {
            if let Some(Value::Array(variants)) = object.get_mut(key) {
                for variant in variants {
                    sanitize_anthropic_schema(variant);
                }
            }
        }
    }
}

/// Build tool definitions, optionally normalizing names. Returns the original
/// tool (name, description) pairs for reverse-mapping on response.
fn build_tools(
    body: &mut serde_json::Value,
    request: &CompletionRequest,
    is_oauth: bool,
    cache_control: &Option<serde_json::Value>,
) -> Vec<(String, String)> {
    if request.tools.is_empty() {
        return Vec::new();
    }

    let original_tools: Vec<(String, String)> = request
        .tools
        .iter()
        .map(|t| (t.name.clone(), t.description.clone()))
        .collect();

    let tool_values: Vec<serde_json::Value> = request
        .tools
        .iter()
        .enumerate()
        .map(|(index, t)| {
            let name = if is_oauth {
                tools::to_claude_code_name(&t.name)
            } else {
                t.name.clone()
            };

            let mut tool = serde_json::json!({
                "name": name,
                "description": t.description,
                "input_schema": t.parameters,
            });

            // Attach cache_control to the last tool definition
            if index == request.tools.len() - 1
                && let Some(cc) = cache_control
            {
                tool["cache_control"] = cc.clone();
            }

            tool
        })
        .collect();

    body["tools"] = serde_json::json!(tool_values);

    original_tools
}

#[cfg(test)]
mod tests {
    use super::*;
    use schemars::JsonSchema;

    #[test]
    fn adaptive_thinking_detected_for_4_6_models() {
        assert!(supports_adaptive_thinking("claude-opus-4-6-20250901"));
        assert!(supports_adaptive_thinking("opus-4.6"));
        assert!(supports_adaptive_thinking("anthropic/claude-opus-4-6"));
        assert!(supports_adaptive_thinking("claude-sonnet-4-6-20250901"));
        assert!(supports_adaptive_thinking("sonnet-4.6"));
        assert!(supports_adaptive_thinking("anthropic/claude-sonnet-4-6"));
    }

    #[test]
    fn adaptive_thinking_not_detected_for_older_models() {
        assert!(!supports_adaptive_thinking("claude-sonnet-4-5"));
        assert!(!supports_adaptive_thinking("claude-opus-4-0"));
        assert!(!supports_adaptive_thinking("gpt-4o"));
    }

    #[allow(dead_code)]
    #[derive(JsonSchema)]
    struct AnthropicOptionalSchema {
        answer: String,
        optional_note: Option<String>,
    }

    #[test]
    fn anthropic_schema_sanitizer_preserves_optional_fields() {
        let mut schema = schemars::schema_for!(AnthropicOptionalSchema).to_value();
        sanitize_anthropic_schema(&mut schema);

        assert_eq!(schema["required"], serde_json::json!(["answer"]));
    }
}
