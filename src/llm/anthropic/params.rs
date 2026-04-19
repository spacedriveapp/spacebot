//! Build full Anthropic API requests with auth, system prompt, thinking, and tools.

use super::auth::{self, AnthropicAuthPath};
use super::cache;
use super::tools;

use reqwest::RequestBuilder;
use rig::completion::CompletionRequest;

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
    force_bearer: bool,
) -> AnthropicRequest {
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
        body["output_config"] = serde_json::json!({ "effort": effort });
    }

    let builder = http_client
        .post(&url)
        .header("anthropic-version", "2023-06-01")
        .header("content-type", "application/json");

    let (builder, auth_path) = auth::apply_auth_headers(builder, api_key, false, force_bearer);
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
        if let Some((stable_prefix, volatile_suffix)) =
            crate::prompts::engine::split_system_prompt_cache_boundary(preamble)
        {
            push_system_text_block(&mut system_blocks, stable_prefix, cache_control);
            push_system_text_block(&mut system_blocks, volatile_suffix, &None);
        } else {
            push_system_text_block(&mut system_blocks, preamble, cache_control);
        }
    }

    if !system_blocks.is_empty() {
        body["system"] = serde_json::json!(system_blocks);
    }
}

fn push_system_text_block(
    system_blocks: &mut Vec<serde_json::Value>,
    text: &str,
    cache_control: &Option<serde_json::Value>,
) {
    if text.trim().is_empty() {
        return;
    }

    let mut block = serde_json::json!({
        "type": "text",
        "text": text,
    });
    if let Some(cache_control) = cache_control {
        block["cache_control"] = cache_control.clone();
    }
    system_blocks.push(block);
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
    use rig::completion::{Message, ToolDefinition};
    use rig::one_or_many::OneOrMany;

    fn completion_request_with_preamble(preamble: &str) -> CompletionRequest {
        CompletionRequest {
            model: None,
            preamble: Some(preamble.to_string()),
            chat_history: OneOrMany::one(Message::user("hello")),
            documents: Vec::new(),
            tools: Vec::new(),
            temperature: None,
            max_tokens: None,
            tool_choice: None,
            additional_params: None,
            output_schema: None,
        }
    }

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

    #[test]
    fn system_prompt_cache_boundary_splits_preamble_cache_control() {
        let request = completion_request_with_preamble(&format!(
            "stable prefix\n{}\nvolatile suffix",
            crate::prompts::engine::SYSTEM_PROMPT_CACHE_BOUNDARY
        ));
        let expected_cache_control = serde_json::json!({"type": "ephemeral"});
        let cache_control = Some(expected_cache_control.clone());
        let mut body = serde_json::json!({});

        build_system_prompt(&mut body, &request, false, &cache_control);

        let system_blocks = body["system"]
            .as_array()
            .expect("system prompt should be an array");
        assert_eq!(system_blocks.len(), 2);
        assert_eq!(system_blocks[0]["text"], "stable prefix\n");
        assert_eq!(system_blocks[0]["cache_control"], expected_cache_control);
        assert_eq!(system_blocks[1]["text"], "\nvolatile suffix");
        assert!(system_blocks[1].get("cache_control").is_none());
    }

    #[test]
    fn system_prompt_without_cache_boundary_preserves_existing_cache_behavior() {
        let request = completion_request_with_preamble("stable prompt");
        let expected_cache_control = serde_json::json!({"type": "ephemeral"});
        let cache_control = Some(expected_cache_control.clone());
        let mut body = serde_json::json!({});

        build_system_prompt(&mut body, &request, false, &cache_control);

        let system_blocks = body["system"]
            .as_array()
            .expect("system prompt should be an array");
        assert_eq!(system_blocks.len(), 1);
        assert_eq!(system_blocks[0]["text"], "stable prompt");
        assert_eq!(system_blocks[0]["cache_control"], expected_cache_control);
    }

    #[test]
    fn build_anthropic_request_keeps_cache_boundary_out_of_volatile_system_block() {
        let client = reqwest::Client::new();
        let mut request = completion_request_with_preamble(&format!(
            "stable prefix\n{}\nvolatile suffix",
            crate::prompts::engine::SYSTEM_PROMPT_CACHE_BOUNDARY
        ));
        request.tools = vec![ToolDefinition {
            name: "reply".to_string(),
            description: "Send a reply".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "text": {"type": "string"}
                }
            }),
        }];

        let anthropic_request = build_anthropic_request(
            &client,
            "sk-ant-test",
            "https://api.anthropic.com",
            "claude-sonnet-4-5",
            &request,
            "auto",
            false,
        );
        let http_request = anthropic_request
            .builder
            .build()
            .expect("request should build");
        let body = http_request
            .body()
            .and_then(reqwest::Body::as_bytes)
            .expect("request body should be buffered JSON");
        let body: serde_json::Value =
            serde_json::from_slice(body).expect("request body should be JSON");

        let system_blocks = body["system"]
            .as_array()
            .expect("system prompt should be an array");
        assert_eq!(system_blocks.len(), 2);
        assert!(system_blocks[0]["cache_control"].is_object());
        assert!(system_blocks[1].get("cache_control").is_none());
        assert_eq!(system_blocks[0]["text"], "stable prefix\n");
        assert_eq!(system_blocks[1]["text"], "\nvolatile suffix");

        let tools = body["tools"]
            .as_array()
            .expect("tool definitions should be an array");
        assert_eq!(tools.len(), 1);
        assert!(tools[0]["cache_control"].is_object());
    }
}
