//! Native Gemini API integration via `gemini-rust`.
//!
//! Converts between Spacebot's rig-based completion types and the native
//! Gemini `generateContent` / `streamGenerateContent` API. This replaces
//! the previous OpenAI-compatible shim that routed through
//! `/v1beta/openai/chat/completions`.

use crate::config::ProviderConfig;
use crate::llm::model::{RawResponse, RawStreamingResponse};

use futures::StreamExt as _;
use gemini_rust::generation::model::GenerationConfig;
use gemini_rust::{
    Content, Gemini, GenerationResponse, Message as GeminiMessage, Part, Role, Tool, UsageMetadata,
};
use rig::completion::{self, CompletionError, CompletionRequest};
use rig::message::{AssistantContent, Message, ToolResultContent};
use rig::one_or_many::OneOrMany;
use rig::streaming::{
    RawStreamingChoice, RawStreamingToolCall, StreamingCompletionResponse, StreamingResult,
};

/// Build a native `gemini_rust::Gemini` client from provider configuration.
///
/// The model name is expected to be the bare name (e.g. `gemini-2.5-flash`),
/// not the `models/` prefixed form — `gemini-rust` accepts either via
/// `Model::Custom`.
pub fn build_client(
    provider_config: &ProviderConfig,
    model_name: &str,
) -> Result<Gemini, CompletionError> {
    // gemini-rust expects model names prefixed with "models/" for custom models.
    let model_str = if model_name.starts_with("models/") {
        model_name.to_string()
    } else {
        format!("models/{model_name}")
    };

    let mut client = Gemini::with_model(&provider_config.api_key, model_str)
        .map_err(|error| CompletionError::ProviderError(format!("Gemini client error: {error}")))?;
        
    if let Some(base_url) = &provider_config.base_url {
        client = client.with_base_url(base_url.clone());
    }

    Ok(client)
}

/// Execute a non-streaming completion request against the native Gemini API.
pub async fn call_gemini(
    client: &Gemini,
    request: &CompletionRequest,
    thinking_effort: &str,
) -> Result<completion::CompletionResponse<RawResponse>, CompletionError> {
    let builder = build_content_request(client, request, thinking_effort)?;

    let response = builder
        .execute()
        .await
        .map_err(|error| CompletionError::ProviderError(format!("Gemini API error: {error}")))?;

    convert_response(response)
}

/// Execute a streaming completion request against the native Gemini API.
pub async fn stream_gemini(
    client: &Gemini,
    request: &CompletionRequest,
    thinking_effort: &str,
) -> Result<StreamingCompletionResponse<RawStreamingResponse>, CompletionError> {
    let builder = build_content_request(client, request, thinking_effort)?;

    let stream = builder
        .execute_stream()
        .await
        .map_err(|error| CompletionError::ProviderError(format!("Gemini stream error: {error}")))?;

    convert_stream(stream)
}

// ---------------------------------------------------------------------------
// Request construction
// ---------------------------------------------------------------------------

/// Build a `ContentBuilder` from a rig `CompletionRequest`.
///
/// Maps the preamble, chat history, tools, and generation config to
/// gemini-rust's builder API.
fn build_content_request(
    client: &Gemini,
    request: &CompletionRequest,
    thinking_effort: &str,
) -> Result<gemini_rust::generation::ContentBuilder, CompletionError> {
    let mut builder = client.generate_content();

    // System prompt
    if let Some(preamble) = &request.preamble {
        builder = builder.with_system_prompt(preamble);
    }

    // Chat history — OneOrMany doesn't implement Iterator on &, use .iter()
    for message in request.chat_history.iter() {
        builder = append_message(builder, message)?;
    }

    // Tools — FunctionDeclaration.parameters is pub(crate), so we serialize
    // the declaration with parameters injected via serde round-trip.
    if !request.tools.is_empty() {
        let function_declarations: Vec<serde_json::Value> = request
            .tools
            .iter()
            .map(|tool_def| {
                let mut declaration = serde_json::json!({
                    "name": tool_def.name,
                    "description": tool_def.description,
                });
                if tool_def.parameters != serde_json::Value::Null {
                    declaration["parameters"] = tool_def.parameters.clone();
                }
                declaration
            })
            .collect();

        // Build the Tool as JSON and deserialize — this bypasses the pub(crate) restriction
        let tool_json = serde_json::json!({
            "function_declarations": function_declarations,
        });
        let tool: Tool = serde_json::from_value(tool_json).map_err(|error| {
            CompletionError::ProviderError(format!("failed to build Gemini tool: {error}"))
        })?;
        builder = builder.with_tool(tool);
    }

    // Generation config (temperature, max tokens, thinking)
    let mut generation_config = GenerationConfig {
        temperature: request.temperature.map(|t| t as f32),
        max_output_tokens: request.max_tokens.map(|t| t as i32),
        ..Default::default()
    };

    // Thinking effort
    if let Some(config) = resolve_thinking_config(thinking_effort) {
        generation_config.thinking_config = Some(config);
    }

    builder = builder.with_generation_config(generation_config);

    Ok(builder)
}

/// Append a rig `Message` to the gemini-rust `ContentBuilder`.
fn append_message(
    builder: gemini_rust::generation::ContentBuilder,
    message: &Message,
) -> Result<gemini_rust::generation::ContentBuilder, CompletionError> {
    match message {
        Message::System { content } => {
            // System messages go into system_instruction — but we already handle
            // preamble above. If there are additional system messages in history,
            // treat them as user messages.
            Ok(builder.with_user_message(content))
        }
        Message::User { content } => {
            // User messages can contain text and tool results
            let mut text_parts = Vec::new();
            let mut tool_responses = Vec::new();

            for item in content.iter() {
                match item {
                    rig::message::UserContent::Text(text) => {
                        text_parts.push(text.text.clone());
                    }
                    rig::message::UserContent::ToolResult(tool_result) => {
                        // Extract the text from the tool result content
                        let result_text = tool_result
                            .content
                            .iter()
                            .filter_map(|part| match part {
                                ToolResultContent::Text(text) => Some(text.text.clone()),
                                _ => None,
                            })
                            .collect::<Vec<_>>()
                            .join("\n");

                        let result_json: serde_json::Value = serde_json::from_str(&result_text)
                            .unwrap_or_else(|_| serde_json::json!({ "result": result_text }));

                        tool_responses.push((tool_result.id.clone(), result_json));
                    }
                    _ => {
                        return Err(CompletionError::RequestError(
                            "Gemini multimodal input is not yet supported in Spacebot"
                                .to_string()
                                .into(),
                        ));
                    }
                }
            }

            let mut current_builder = builder;

            // Add text as user message
            if !text_parts.is_empty() {
                let combined_text = text_parts.join("\n");
                current_builder = current_builder.with_user_message(combined_text);
            }

            // Add tool responses as function responses
            for (name, result_json) in tool_responses {
                let function_response_content = Content::function_response_json(&name, result_json);
                let gemini_message = GeminiMessage {
                    content: function_response_content.with_role(Role::User),
                    role: Role::User,
                };
                current_builder = current_builder.with_message(gemini_message);
            }

            Ok(current_builder)
        }
        Message::Assistant { content, .. } => {
            // Process assistant content — text and tool calls
            let mut parts: Vec<Part> = Vec::new();

            for item in content.iter() {
                match item {
                    AssistantContent::Text(text) => {
                        parts.push(Part::Text {
                            text: text.text.clone(),
                            thought: None,
                            thought_signature: None,
                        });
                    }
                    AssistantContent::ToolCall(tool_call) => {
                        parts.push(Part::FunctionCall {
                            function_call: gemini_rust::FunctionCall::new(
                                tool_call.function.name.clone(),
                                tool_call.function.arguments.clone(),
                            ),
                            thought_signature: tool_call.signature.clone(),
                        });
                    }
                    _ => {}
                }
            }

            if !parts.is_empty() {
                let content = Content {
                    parts: Some(parts),
                    role: Some(Role::Model),
                };
                let gemini_message = GeminiMessage {
                    content,
                    role: Role::Model,
                };
                Ok(builder.with_message(gemini_message))
            } else {
                Ok(builder)
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Response conversion
// ---------------------------------------------------------------------------

/// Convert a native Gemini `GenerationResponse` to a rig `CompletionResponse`.
fn convert_response(
    response: GenerationResponse,
) -> Result<completion::CompletionResponse<RawResponse>, CompletionError> {
    let usage = response
        .usage_metadata
        .as_ref()
        .map(convert_usage)
        .unwrap_or_default();

    // Serialize the full response for RawResponse
    let body = serde_json::to_value(&response).map_err(|e| {
        CompletionError::ResponseError(
            format!("Failed to serialize Gemini response for accounting: {e}").into(),
        )
    })?;

    // Extract text and tool calls from the first candidate
    let mut choice_content: Vec<AssistantContent> = Vec::new();

    // Text parts (non-thought only)
    for (text, is_thought) in response.all_text() {
        if !is_thought {
            choice_content.push(AssistantContent::text(text));
        }
    }

    // Function calls
    for function_call in response.function_calls() {
        let id = format!("call_{}", uuid::Uuid::new_v4().simple());
        choice_content.push(AssistantContent::tool_call(
            id,
            function_call.name.clone(),
            function_call.args.clone(),
        ));
    }

    let choice = if choice_content.is_empty() {
        OneOrMany::one(AssistantContent::text(String::new()))
    } else {
        OneOrMany::many(choice_content)
            .unwrap_or_else(|_| OneOrMany::one(AssistantContent::text(String::new())))
    };

    Ok(completion::CompletionResponse {
        choice,
        raw_response: RawResponse { body },
        usage,
        message_id: None,
    })
}

/// Convert a Gemini streaming response into a rig `StreamingCompletionResponse`.
fn convert_stream(
    stream: gemini_rust::GenerationStream,
) -> Result<StreamingCompletionResponse<RawStreamingResponse>, CompletionError> {
    // Track accumulated state across chunks for the final response
    let accumulated_body = std::sync::Arc::new(std::sync::Mutex::new(serde_json::Value::Null));
    let accumulated_usage = std::sync::Arc::new(std::sync::Mutex::new(None::<completion::Usage>));

    let body_for_final = accumulated_body.clone();
    let usage_for_final = accumulated_usage.clone();

    let mapped_stream = async_stream::try_stream! {
        let mut stream = std::pin::pin!(stream);

        while let Some(result) = stream.next().await {
            let chunk = result.map_err(|error| {
                CompletionError::ProviderError(format!("Gemini stream error: {error}"))
            })?;

            // Update usage from this chunk if present
            if let Some(usage_meta) = &chunk.usage_metadata {
                let usage = convert_usage(usage_meta);
                if let Ok(mut guard) = accumulated_usage.try_lock() {
                    *guard = Some(usage);
                }
                if let Ok(mut guard) = accumulated_body.try_lock() {
                    match serde_json::to_value(&chunk) {
                        Ok(val) => *guard = val,
                        Err(e) => {
                            tracing::warn!("Failed to serialize Gemini chunk for accounting: {e}")
                        }
                    }
                }
            }

            // Text parts
            for (text, is_thought) in chunk.all_text() {
                if !is_thought && !text.is_empty() {
                    yield RawStreamingChoice::<RawStreamingResponse>::Message(text);
                }
            }

            // Function calls
            for function_call in chunk.function_calls() {
                let id = format!("call_{}", uuid::Uuid::new_v4().simple());
                yield RawStreamingChoice::<RawStreamingResponse>::ToolCall(
                    RawStreamingToolCall::new(
                        id,
                        function_call.name.clone(),
                        function_call.args.clone(),
                    ),
                );
            }
        }

        // Yield final response with usage info
        let body = body_for_final.lock().unwrap().clone();
        let usage = usage_for_final.lock().unwrap().take();
        yield RawStreamingChoice::FinalResponse(RawStreamingResponse { body, usage });
    };

    let stream: StreamingResult<RawStreamingResponse> = Box::pin(mapped_stream);
    Ok(StreamingCompletionResponse::stream(stream))
}

// ---------------------------------------------------------------------------
// Thinking config
// ---------------------------------------------------------------------------

/// Map Spacebot's thinking effort string to Gemini's `ThinkingConfig`.
fn resolve_thinking_config(effort: &str) -> Option<gemini_rust::generation::model::ThinkingConfig> {
    use gemini_rust::generation::model::{ThinkingConfig, ThinkingLevel};

    match effort.to_lowercase().as_str() {
        "auto" | "" => None,
        "none" | "off" => Some(ThinkingConfig::new().with_thinking_budget(0)),
        "low" => Some(ThinkingConfig::new().with_thinking_level(ThinkingLevel::Low)),
        "medium" => Some(ThinkingConfig::new().with_thinking_level(ThinkingLevel::Medium)),
        "high" => Some(ThinkingConfig::new().with_thinking_level(ThinkingLevel::High)),
        "minimal" => Some(ThinkingConfig::new().with_thinking_level(ThinkingLevel::Minimal)),
        _ => {
            // Try parsing as a numeric budget
            if let Ok(budget) = effort.parse::<i32>() {
                Some(ThinkingConfig::new().with_thinking_budget(budget))
            } else {
                tracing::warn!(effort, "unknown Gemini thinking effort, ignoring");
                None
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Usage conversion
// ---------------------------------------------------------------------------

/// Convert Gemini `UsageMetadata` to rig's `completion::Usage`.
fn convert_usage(usage: &UsageMetadata) -> completion::Usage {
    let input_tokens = usage.prompt_token_count.unwrap_or(0) as u64;
    let output_tokens = usage.candidates_token_count.unwrap_or(0) as u64;
    let cached_input_tokens = usage.cached_content_token_count.unwrap_or(0) as u64;
    completion::Usage {
        input_tokens,
        output_tokens,
        total_tokens: input_tokens + output_tokens,
        cached_input_tokens,
    }
}
