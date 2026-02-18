//! Build full Anthropic API requests with auth, system prompt, thinking, and tools.

use super::auth::{self, AnthropicAuthPath};
use super::cache;
use super::tools;

use reqwest::RequestBuilder;
use rig::completion::CompletionRequest;

const ANTHROPIC_API_URL: &str = "https://api.anthropic.com/v1/messages";
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

/// Build a fully configured Anthropic API request from a CompletionRequest.
///
/// `thinking_effort` controls adaptive thinking: "auto" picks max for Opus /
/// high for others, or pass "max", "high", "medium", "low" explicitly.
pub fn build_anthropic_request(
    http_client: &reqwest::Client,
    api_key: &str,
    model_name: &str,
    request: &CompletionRequest,
    thinking_effort: &str,
) -> AnthropicRequest {
    let is_oauth = auth::detect_auth_path(api_key) == AnthropicAuthPath::OAuthToken;
    let adaptive_thinking = supports_adaptive_thinking(model_name);
    let retention = cache::resolve_cache_retention(None);
    let cache_control = cache::get_cache_control(ANTHROPIC_API_URL, retention);

    let mut body = serde_json::json!({
        "model": model_name,
        "max_tokens": request.max_tokens.unwrap_or(16_000),
    });

    build_system_prompt(&mut body, request, is_oauth, &cache_control);

    let messages = crate::llm::model::convert_messages_to_anthropic(&request.chat_history);
    body["messages"] = serde_json::json!(messages);

    let original_tools = build_tools(&mut body, request, is_oauth, &cache_control);

    if let Some(temperature) = request.temperature {
        body["temperature"] = serde_json::json!(temperature);
    }

    if adaptive_thinking {
        body["thinking"] = serde_json::json!({ "type": "adaptive" });
        let effort = match thinking_effort {
            "max" | "high" | "medium" | "low" => thinking_effort,
            _ => if is_opus(model_name) { "max" } else { "high" },
        };
        body["output_config"] = serde_json::json!({ "effort": effort });
    }

    let builder = http_client
        .post(ANTHROPIC_API_URL)
        .header("anthropic-version", "2023-06-01")
        .header("content-type", "application/json");

    let (builder, auth_path) = auth::apply_auth_headers(builder, api_key, false);
    let builder = builder.json(&body);

    AnthropicRequest {
        builder,
        auth_path,
        original_tools,
    }
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
            if index == request.tools.len() - 1 {
                if let Some(cc) = cache_control {
                    tool["cache_control"] = cc.clone();
                }
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
}
