//! SpacebotModel: Custom CompletionModel implementation that routes through LlmManager.

use crate::config::{ApiType, ProviderConfig};
use crate::llm::manager::LlmManager;
use crate::llm::routing::{
    self, MAX_FALLBACK_ATTEMPTS, MAX_RETRIES_PER_MODEL, RETRY_BASE_DELAY_MS, RoutingConfig,
};

use futures::StreamExt as _;
use rig::completion::{self, CompletionError, CompletionModel, CompletionRequest, GetTokenUsage};
use rig::message::{
    AssistantContent, DocumentSourceKind, Image, Message, MimeType, Text, ToolCall, ToolFunction,
    UserContent,
};
use rig::one_or_many::OneOrMany;
use rig::streaming::{RawStreamingChoice, RawStreamingToolCall, StreamingCompletionResponse};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::Arc;

const STREAM_REQUEST_TIMEOUT_SECS: u64 = 30 * 60;

/// Raw provider response. Wraps the JSON so Rig can carry it through.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RawResponse {
    pub body: serde_json::Value,
}

/// Streaming response wrapper for token usage and raw provider payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RawStreamingResponse {
    pub body: serde_json::Value,
    pub usage: Option<completion::Usage>,
}

impl GetTokenUsage for RawStreamingResponse {
    fn token_usage(&self) -> Option<completion::Usage> {
        self.usage
    }
}

/// Custom completion model that routes through LlmManager.
///
/// Optionally holds a RoutingConfig for fallback behavior. When present,
/// completion() will try fallback models on retriable errors.
#[derive(Clone)]
pub struct SpacebotModel {
    llm_manager: Arc<LlmManager>,
    model_name: String,
    provider: String,
    full_model_name: String,
    routing: Option<RoutingConfig>,
    agent_id: Option<String>,
    process_type: Option<String>,
    worker_type: Option<String>,
}

impl SpacebotModel {
    pub fn provider(&self) -> &str {
        &self.provider
    }
    pub fn model_name(&self) -> &str {
        &self.model_name
    }
    pub fn full_model_name(&self) -> &str {
        &self.full_model_name
    }

    /// Attach routing config for fallback behavior.
    pub fn with_routing(mut self, routing: RoutingConfig) -> Self {
        self.routing = Some(routing);
        self
    }

    /// Attach agent context for per-agent metric labels.
    pub fn with_context(
        mut self,
        agent_id: impl Into<String>,
        process_type: impl Into<String>,
    ) -> Self {
        self.agent_id = Some(agent_id.into());
        self.process_type = Some(process_type.into());
        self
    }

    /// Attach a worker type label for metrics (e.g. "builtin", "opencode").
    pub fn with_worker_type(mut self, worker_type: impl Into<String>) -> Self {
        self.worker_type = Some(worker_type.into());
        self
    }

    async fn provider_config_for_current_model(&self) -> Result<ProviderConfig, CompletionError> {
        let provider_id = self
            .full_model_name
            .split_once('/')
            .map(|(provider, _)| provider)
            .unwrap_or("anthropic");

        match provider_id {
            "anthropic" => self
                .llm_manager
                .get_anthropic_provider()
                .await
                .map_err(|error| CompletionError::ProviderError(error.to_string())),
            "openai" => self
                .llm_manager
                .get_openai_provider()
                .await
                .map_err(|error| CompletionError::ProviderError(error.to_string())),
            "openai-chatgpt" => self
                .llm_manager
                .get_openai_chatgpt_provider()
                .await
                .map_err(|error| CompletionError::ProviderError(error.to_string())),
            _ => self
                .llm_manager
                .get_provider(provider_id)
                .map_err(|error| CompletionError::ProviderError(error.to_string())),
        }
    }

    /// Direct call to the provider (no fallback logic).
    async fn attempt_completion(
        &self,
        request: CompletionRequest,
    ) -> Result<completion::CompletionResponse<RawResponse>, CompletionError> {
        let provider_config = self.provider_config_for_current_model().await?;

        match provider_config.api_type {
            ApiType::Anthropic => self.call_anthropic(request, &provider_config).await,
            ApiType::OpenAiCompletions => self.call_openai(request, &provider_config).await,
            ApiType::OpenAiChatCompletions => {
                let endpoint = format!(
                    "{}/chat/completions",
                    provider_config.base_url.trim_end_matches('/')
                );
                let display_name = provider_config
                    .name
                    .as_deref()
                    .unwrap_or("OpenAI-compatible provider");
                let headers: Vec<(&str, &str)> = provider_config
                    .extra_headers
                    .iter()
                    .map(|(k, v)| (k.as_str(), v.as_str()))
                    .collect();
                self.call_openai_compatible_with_optional_auth(
                    request,
                    display_name,
                    &endpoint,
                    Some(provider_config.api_key.clone()),
                    &headers,
                )
                .await
            }
            ApiType::KiloGateway => {
                let endpoint = format!(
                    "{}/chat/completions",
                    provider_config.base_url.trim_end_matches('/')
                );
                self.call_openai_compatible_with_optional_auth(
                    request,
                    "Kilo Gateway",
                    &endpoint,
                    Some(provider_config.api_key.clone()),
                    &[
                        ("HTTP-Referer", "https://github.com/spacedriveapp/spacebot"),
                        ("X-Title", "spacebot"),
                    ],
                )
                .await
            }
            ApiType::OpenAiResponses => self.call_openai_responses(request, &provider_config).await,
            ApiType::Gemini => {
                self.call_openai_compatible(request, "Google Gemini", &provider_config)
                    .await
            }
        }
    }

    /// Try a model with retries and exponential backoff on transient errors.
    ///
    /// Returns `Ok(response)` on success, or `Err((last_error, was_rate_limit))`
    /// after exhausting retries. `was_rate_limit` indicates the final failure was
    /// a 429/rate-limit (as opposed to a timeout or server error), so the caller
    /// can decide whether to record cooldown.
    async fn attempt_with_retries(
        &self,
        model_name: &str,
        request: &CompletionRequest,
    ) -> Result<completion::CompletionResponse<RawResponse>, (CompletionError, bool)> {
        let model = if model_name == self.full_model_name {
            self.clone()
        } else {
            SpacebotModel::make(&self.llm_manager, model_name)
        };

        let mut last_error = None;
        for attempt in 0..MAX_RETRIES_PER_MODEL {
            if attempt > 0 {
                let delay_ms = RETRY_BASE_DELAY_MS * 2u64.pow((attempt - 1) as u32);
                tracing::debug!(
                    model = %model_name,
                    attempt = attempt + 1,
                    delay_ms,
                    "retrying after backoff"
                );
                tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
            }

            match model.attempt_completion(request.clone()).await {
                Ok(response) => return Ok(response),
                Err(error) => {
                    let error_str = error.to_string();
                    if !routing::is_retriable_error(&error_str) {
                        // Non-retriable (auth error, bad request, etc) — bail immediately
                        return Err((error, false));
                    }
                    tracing::warn!(
                        model = %model_name,
                        attempt = attempt + 1,
                        %error,
                        "retriable error"
                    );
                    last_error = Some(error_str);
                }
            }
        }

        let error_str = last_error.unwrap_or_default();
        let was_rate_limit = routing::is_rate_limit_error(&error_str);
        Err((
            CompletionError::ProviderError(format!(
                "{model_name} failed after {MAX_RETRIES_PER_MODEL} attempts: {error_str}"
            )),
            was_rate_limit,
        ))
    }
}

impl CompletionModel for SpacebotModel {
    type Response = RawResponse;
    type StreamingResponse = RawStreamingResponse;
    type Client = Arc<LlmManager>;

    fn make(client: &Self::Client, model: impl Into<String>) -> Self {
        let full_name = model.into();

        // OpenRouter model names have the form "openrouter/provider/model",
        // so split on the first "/" only and keep the rest as the model name.
        let (provider, model_name) = if let Some(rest) = full_name.strip_prefix("openrouter/") {
            ("openrouter".to_string(), rest.to_string())
        } else if let Some((p, m)) = full_name.split_once('/') {
            (p.to_string(), m.to_string())
        } else {
            ("anthropic".to_string(), full_name.clone())
        };

        let full_model_name = format!("{provider}/{model_name}");

        Self {
            llm_manager: client.clone(),
            model_name,
            provider,
            full_model_name,
            routing: None,
            agent_id: None,
            process_type: None,
            worker_type: None,
        }
    }

    async fn completion(
        &self,
        request: CompletionRequest,
    ) -> Result<completion::CompletionResponse<RawResponse>, CompletionError> {
        #[cfg(feature = "metrics")]
        let start = std::time::Instant::now();

        let result = async move {
            let Some(routing) = &self.routing else {
                // No routing config — just call the model directly, no fallback/retry
                return self.attempt_completion(request).await;
            };

            let cooldown = routing.rate_limit_cooldown_secs;
            let fallbacks = routing.get_fallbacks(&self.full_model_name);
            let mut last_error: Option<CompletionError> = None;

            // Try the primary model (with retries) unless it's in rate-limit cooldown
            // and we have fallbacks to try instead.
            let primary_rate_limited = self
                .llm_manager
                .is_rate_limited(&self.full_model_name, cooldown)
                .await;

            let skip_primary = primary_rate_limited && !fallbacks.is_empty();

            if skip_primary {
                tracing::debug!(
                    model = %self.full_model_name,
                    "primary model in rate-limit cooldown, skipping to fallbacks"
                );
            } else {
                match self
                    .attempt_with_retries(&self.full_model_name, &request)
                    .await
                {
                    Ok(response) => return Ok(response),
                    Err((error, was_rate_limit)) => {
                        if was_rate_limit {
                            self.llm_manager
                                .record_rate_limit(&self.full_model_name)
                                .await;
                        }
                        if fallbacks.is_empty() {
                            // No fallbacks — this is the final error
                            return Err(error);
                        }
                        tracing::warn!(
                            model = %self.full_model_name,
                            "primary model exhausted retries, trying fallbacks"
                        );
                        last_error = Some(error);
                    }
                }
            }

            // Try fallback chain, each with their own retry loop
            for (index, fallback_name) in fallbacks.iter().take(MAX_FALLBACK_ATTEMPTS).enumerate() {
                if self
                    .llm_manager
                    .is_rate_limited(fallback_name, cooldown)
                    .await
                {
                    tracing::debug!(
                        fallback = %fallback_name,
                        "fallback model in cooldown, skipping"
                    );
                    continue;
                }

                match self.attempt_with_retries(fallback_name, &request).await {
                    Ok(response) => {
                        tracing::info!(
                            original = %self.full_model_name,
                            fallback = %fallback_name,
                            attempt = index + 1,
                            "fallback model succeeded"
                        );
                        return Ok(response);
                    }
                    Err((error, was_rate_limit)) => {
                        if was_rate_limit {
                            self.llm_manager.record_rate_limit(fallback_name).await;
                        }
                        tracing::warn!(
                            fallback = %fallback_name,
                            "fallback model exhausted retries, continuing chain"
                        );
                        last_error = Some(error);
                    }
                }
            }

            Err(last_error.unwrap_or_else(|| {
                CompletionError::ProviderError("all models in fallback chain failed".into())
            }))
        }
        .await;

        #[cfg(feature = "metrics")]
        {
            let elapsed = start.elapsed().as_secs_f64();
            let agent_label = self.agent_id.as_deref().unwrap_or("unknown");
            let tier_label = self.process_type.as_deref().unwrap_or("unknown");
            let worker_label = match self.worker_type.as_deref() {
                Some(worker_type) => worker_type,
                None if tier_label == "worker" => "unknown",
                None => "",
            };
            let metrics = crate::telemetry::Metrics::global();
            metrics
                .llm_requests_total
                .with_label_values(&[agent_label, &self.full_model_name, tier_label, worker_label])
                .inc();
            metrics
                .llm_request_duration_seconds
                .with_label_values(&[agent_label, &self.full_model_name, tier_label, worker_label])
                .observe(elapsed);

            if let Ok(ref response) = result {
                let usage = &response.usage;
                if usage.input_tokens > 0 || usage.output_tokens > 0 {
                    metrics
                        .llm_tokens_total
                        .with_label_values(&[
                            agent_label,
                            &self.full_model_name,
                            tier_label,
                            "input",
                            worker_label,
                        ])
                        .inc_by(usage.input_tokens);
                    metrics
                        .llm_tokens_total
                        .with_label_values(&[
                            agent_label,
                            &self.full_model_name,
                            tier_label,
                            "output",
                            worker_label,
                        ])
                        .inc_by(usage.output_tokens);
                    if usage.cached_input_tokens > 0 {
                        metrics
                            .llm_tokens_total
                            .with_label_values(&[
                                agent_label,
                                &self.full_model_name,
                                tier_label,
                                "cached_input",
                                worker_label,
                            ])
                            .inc_by(usage.cached_input_tokens);
                    }

                    let cost = crate::llm::pricing::estimate_cost(
                        &self.full_model_name,
                        usage.input_tokens,
                        usage.output_tokens,
                        usage.cached_input_tokens,
                    );
                    if cost > 0.0 {
                        metrics
                            .llm_estimated_cost_dollars
                            .with_label_values(&[
                                agent_label,
                                &self.full_model_name,
                                tier_label,
                                worker_label,
                            ])
                            .inc_by(cost);

                        // Track per-worker cost separately when this is a worker process.
                        if tier_label == "worker" {
                            metrics
                                .worker_cost_dollars
                                .with_label_values(&[agent_label, worker_label])
                                .inc_by(cost);
                        }
                    }
                }
            }

            if let Err(ref error) = result {
                let error_type = match error {
                    rig::completion::CompletionError::ProviderError(msg) => {
                        if msg.contains("rate") || msg.contains("429") {
                            "rate_limit"
                        } else if msg.contains("timeout") {
                            "timeout"
                        } else if msg.contains("context") || msg.contains("too long") {
                            "context_overflow"
                        } else {
                            "provider_error"
                        }
                    }
                    _ => "other",
                };
                metrics
                    .process_errors_total
                    .with_label_values(&[agent_label, tier_label, error_type, worker_label])
                    .inc();

                if error_type == "context_overflow" {
                    metrics
                        .context_overflow_total
                        .with_label_values(&[agent_label, tier_label])
                        .inc();
                }
            }
        }

        result
    }

    async fn stream(
        &self,
        request: CompletionRequest,
    ) -> Result<StreamingCompletionResponse<RawStreamingResponse>, CompletionError> {
        let provider_config = self.provider_config_for_current_model().await?;

        match provider_config.api_type {
            ApiType::OpenAiCompletions => self.stream_openai(request, &provider_config).await,
            ApiType::OpenAiChatCompletions => {
                let endpoint = format!(
                    "{}/chat/completions",
                    provider_config.base_url.trim_end_matches('/')
                );
                let display_name = provider_config
                    .name
                    .as_deref()
                    .unwrap_or("OpenAI-compatible provider");
                let headers: Vec<(&str, &str)> = provider_config
                    .extra_headers
                    .iter()
                    .map(|(key, value)| (key.as_str(), value.as_str()))
                    .collect();
                self.stream_openai_compatible_with_optional_auth(
                    request,
                    display_name,
                    &endpoint,
                    Some(provider_config.api_key.clone()),
                    &headers,
                )
                .await
            }
            ApiType::KiloGateway => {
                let endpoint = format!(
                    "{}/chat/completions",
                    provider_config.base_url.trim_end_matches('/')
                );
                self.stream_openai_compatible_with_optional_auth(
                    request,
                    "Kilo Gateway",
                    &endpoint,
                    Some(provider_config.api_key.clone()),
                    &[
                        ("HTTP-Referer", "https://github.com/spacedriveapp/spacebot"),
                        ("X-Title", "spacebot"),
                    ],
                )
                .await
            }
            ApiType::Gemini => {
                self.stream_openai_compatible(request, "Google Gemini", &provider_config)
                    .await
            }
            ApiType::Anthropic | ApiType::OpenAiResponses => {
                let response = self.attempt_completion(request).await?;
                Ok(stream_from_completion_response(response))
            }
        }
    }
}

impl SpacebotModel {
    async fn call_anthropic(
        &self,
        request: CompletionRequest,
        provider_config: &ProviderConfig,
    ) -> Result<completion::CompletionResponse<RawResponse>, CompletionError> {
        let api_key = provider_config.api_key.as_str();

        let effort = self
            .routing
            .as_ref()
            .map(|r| r.thinking_effort_for_model(&self.model_name))
            .unwrap_or("auto");
        let anthropic_request = crate::llm::anthropic::build_anthropic_request(
            self.llm_manager.http_client(),
            api_key,
            &provider_config.base_url,
            &self.model_name,
            &request,
            effort,
            provider_config.use_bearer_auth,
        );

        let is_oauth =
            anthropic_request.auth_path == crate::llm::anthropic::AnthropicAuthPath::OAuthToken;
        let original_tools = anthropic_request.original_tools;

        let response = anthropic_request
            .builder
            .send()
            .await
            .map_err(|e| CompletionError::ProviderError(e.to_string()))?;

        let status = response.status();
        let response_text = response.text().await.map_err(|e| {
            CompletionError::ProviderError(format!("failed to read response body: {e}"))
        })?;

        let response_body: serde_json::Value =
            serde_json::from_str(&response_text).map_err(|e| {
                CompletionError::ProviderError(format!(
                    "Anthropic response ({status}) is not valid JSON: {e}\nBody: {}",
                    truncate_body(&response_text)
                ))
            })?;

        if !status.is_success() {
            let message = response_body["error"]["message"]
                .as_str()
                .unwrap_or("unknown error");
            return Err(CompletionError::ProviderError(format!(
                "Anthropic API error ({status}): {message}"
            )));
        }

        let mut completion = parse_anthropic_response(response_body)?;

        // Reverse-map tool names when using OAuth (Claude Code canonical → original)
        if is_oauth && !original_tools.is_empty() {
            reverse_map_tool_names(&mut completion, &original_tools);
        }

        Ok(completion)
    }

    async fn call_openai(
        &self,
        request: CompletionRequest,
        provider_config: &ProviderConfig,
    ) -> Result<completion::CompletionResponse<RawResponse>, CompletionError> {
        let stream = self.stream_openai(request, provider_config).await?;
        collect_streaming_completion_response(stream).await
    }

    async fn stream_openai(
        &self,
        request: CompletionRequest,
        provider_config: &ProviderConfig,
    ) -> Result<StreamingCompletionResponse<RawStreamingResponse>, CompletionError> {
        let api_key = provider_config.api_key.as_str();
        let provider_label = provider_config
            .name
            .as_deref()
            .filter(|name| !name.trim().is_empty())
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| provider_display_name(&self.provider));

        let mut messages = Vec::new();

        if let Some(preamble) = &request.preamble {
            messages.push(serde_json::json!({
                "role": "system",
                "content": preamble,
            }));
        }

        messages.extend(convert_messages_to_openai(&request.chat_history));

        let api_model_name = self.remap_model_name_for_api();
        let mut body = serde_json::json!({
            "model": api_model_name,
            "messages": messages,
        });

        if let Some(max_tokens) = request.max_tokens {
            body["max_tokens"] = serde_json::json!(max_tokens);
        }

        if let Some(temperature) = request.temperature {
            body["temperature"] = serde_json::json!(temperature);
        }

        if !request.tools.is_empty() {
            let tools: Vec<serde_json::Value> = request
                .tools
                .iter()
                .map(|t| {
                    serde_json::json!({
                        "type": "function",
                        "function": {
                            "name": t.name,
                            "description": t.description,
                            "parameters": t.parameters,
                        }
                    })
                })
                .collect();
            body["tools"] = serde_json::json!(tools);
        }

        let chat_completions_url = format!(
            "{}/v1/chat/completions",
            provider_config.base_url.trim_end_matches('/')
        );
        let openai_account_id = if self.provider == "openai-chatgpt" {
            self.llm_manager.get_openai_account_id().await
        } else {
            None
        };

        let http_client = self.llm_manager.http_client().clone();
        let auth_header = format!("Bearer {api_key}");
        let extra_headers = provider_config.extra_headers.clone();
        let is_kimi_endpoint = chat_completions_url.contains("kimi.com")
            || chat_completions_url.contains("moonshot.ai");

        self.stream_openai_chat_request(
            move |request_body| {
                let mut request_builder = http_client
                    .post(&chat_completions_url)
                    .header("authorization", auth_header.clone())
                    .header("content-type", "application/json");

                if let Some(account_id) = openai_account_id.as_deref() {
                    request_builder = request_builder.header("chatgpt-account-id", account_id);
                }

                if is_kimi_endpoint {
                    request_builder = request_builder.header("user-agent", "KimiCLI/1.3");
                }

                for (key, value) in &extra_headers {
                    request_builder = request_builder.header(key, value);
                }

                request_builder.json(request_body)
            },
            body,
            &provider_label,
        )
        .await
    }

    async fn call_openai_responses(
        &self,
        request: CompletionRequest,
        provider_config: &ProviderConfig,
    ) -> Result<completion::CompletionResponse<RawResponse>, CompletionError> {
        let base_url = provider_config.base_url.trim_end_matches('/');
        let is_chatgpt_codex = self.provider == "openai-chatgpt";
        let responses_url = if is_chatgpt_codex {
            format!("{base_url}/responses")
        } else {
            format!("{base_url}/v1/responses")
        };
        let api_key = provider_config.api_key.as_str();
        let provider_label = provider_config
            .name
            .as_deref()
            .filter(|name| !name.trim().is_empty())
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| provider_display_name(&self.provider));

        let input = convert_messages_to_openai_responses(&request.chat_history);

        let api_model_name = self.remap_model_name_for_api();
        let mut body = serde_json::json!({
            "model": api_model_name,
            "input": input,
        });

        let configured_effort = self
            .routing
            .as_ref()
            .map(|r| r.thinking_effort_for_model(&self.model_name))
            .unwrap_or("auto");
        if let Some(effort) = openai_reasoning_effort(&api_model_name, configured_effort) {
            body["reasoning"] = serde_json::json!({ "effort": effort });
        }

        if let Some(preamble) = &request.preamble {
            body["instructions"] = serde_json::json!(preamble);
        } else if is_chatgpt_codex {
            body["instructions"] = serde_json::json!(
                "You are Spacebot. Follow instructions exactly and respond concisely."
            );
        }

        if !is_chatgpt_codex && let Some(max_tokens) = request.max_tokens {
            body["max_output_tokens"] = serde_json::json!(max_tokens);
        }

        if !is_chatgpt_codex && let Some(temperature) = request.temperature {
            body["temperature"] = serde_json::json!(temperature);
        }

        if is_chatgpt_codex {
            body["store"] = serde_json::json!(false);
            body["stream"] = serde_json::json!(true);
        }

        if !request.tools.is_empty() {
            let tools: Vec<serde_json::Value> = request
                .tools
                .iter()
                .map(|tool_definition| {
                    serde_json::json!({
                        "type": "function",
                        "name": tool_definition.name,
                        "description": tool_definition.description,
                        "parameters": tool_definition.parameters,
                    })
                })
                .collect();
            body["tools"] = serde_json::json!(tools);
        }

        let openai_account_id = if self.provider == "openai-chatgpt" {
            self.llm_manager.get_openai_account_id().await
        } else {
            None
        };

        let mut request_builder = self
            .llm_manager
            .http_client()
            .post(&responses_url)
            .header("authorization", format!("Bearer {api_key}"))
            .header("content-type", "application/json");
        if let Some(account_id) = openai_account_id {
            request_builder = request_builder.header("ChatGPT-Account-Id", account_id);
        }
        if is_chatgpt_codex {
            request_builder = request_builder
                .header("originator", "opencode")
                .header(
                    "session_id",
                    format!("spacebot-{}", chrono::Utc::now().timestamp()),
                )
                .header(
                    "user-agent",
                    format!("spacebot/{}", env!("CARGO_PKG_VERSION")),
                );
        }

        let response = request_builder
            .json(&body)
            .send()
            .await
            .map_err(|e| CompletionError::ProviderError(e.to_string()))?;

        let status = response.status();
        let response_text = response.text().await.map_err(|e| {
            CompletionError::ProviderError(format!("failed to read response body: {e}"))
        })?;

        if !status.is_success() {
            let message = parse_openai_error_message(&response_text)
                .unwrap_or_else(|| "unknown error".to_string());
            return Err(CompletionError::ProviderError(format!(
                "{provider_label} Responses API error ({status}): {message}"
            )));
        }

        let response_body: serde_json::Value = if is_chatgpt_codex {
            parse_openai_responses_sse_response(&response_text, &provider_label)?
        } else {
            serde_json::from_str(&response_text).map_err(|e| {
                CompletionError::ProviderError(format!(
                    "{provider_label} Responses API response ({status}) is not valid JSON: {e}\nBody: {}",
                    truncate_body(&response_text)
                ))
            })?
        };

        parse_openai_responses_response(response_body, &provider_label)
    }

    /// Generic OpenAI-compatible API call.
    /// Used by providers that implement the OpenAI chat completions format.
    #[allow(dead_code)]
    async fn call_openai_compatible(
        &self,
        request: CompletionRequest,
        provider_display_name: &str,
        provider_config: &ProviderConfig,
    ) -> Result<completion::CompletionResponse<RawResponse>, CompletionError> {
        let stream = self
            .stream_openai_compatible(request, provider_display_name, provider_config)
            .await?;
        collect_streaming_completion_response(stream).await
    }

    async fn stream_openai_compatible(
        &self,
        request: CompletionRequest,
        provider_display_name: &str,
        provider_config: &ProviderConfig,
    ) -> Result<StreamingCompletionResponse<RawStreamingResponse>, CompletionError> {
        let base_url = provider_config.base_url.trim_end_matches('/');
        let endpoint_path = match provider_config.api_type {
            ApiType::OpenAiCompletions | ApiType::OpenAiResponses => "/v1/chat/completions",
            ApiType::OpenAiChatCompletions | ApiType::Gemini => "/chat/completions",
            ApiType::Anthropic => {
                return Err(CompletionError::ProviderError(format!(
                    "{provider_display_name} is configured with anthropic API type, but this call expects an OpenAI-compatible API"
                )));
            }
            _ => {
                return Err(CompletionError::ProviderError(format!(
                    "{provider_display_name} uses API type {:?} which does not support OpenAI-compatible calls",
                    provider_config.api_type
                )));
            }
        };
        let endpoint = format!("{base_url}{endpoint_path}");
        let api_key = provider_config.api_key.as_str();

        let mut messages = Vec::new();

        if let Some(preamble) = &request.preamble {
            messages.push(serde_json::json!({
                "role": "system",
                "content": preamble,
            }));
        }

        messages.extend(convert_messages_to_openai(&request.chat_history));

        let mut body = serde_json::json!({
            "model": self.model_name,
            "messages": messages,
        });

        if let Some(max_tokens) = request.max_tokens {
            body["max_tokens"] = serde_json::json!(max_tokens);
        }

        if let Some(temperature) = request.temperature {
            body["temperature"] = serde_json::json!(temperature);
        }

        if !request.tools.is_empty() {
            let tools: Vec<serde_json::Value> = request
                .tools
                .iter()
                .map(|t| {
                    serde_json::json!({
                        "type": "function",
                        "function": {
                            "name": t.name,
                            "description": t.description,
                            "parameters": t.parameters,
                        }
                    })
                })
                .collect();
            body["tools"] = serde_json::json!(tools);
        }

        let http_client = self.llm_manager.http_client().clone();
        let auth_header = format!("Bearer {api_key}");
        self.stream_openai_chat_request(
            move |request_body| {
                http_client
                    .post(&endpoint)
                    .header("authorization", auth_header.clone())
                    .header("content-type", "application/json")
                    .json(request_body)
            },
            body,
            provider_display_name,
        )
        .await
    }

    /// Remap model name for providers that require a different format in API calls.
    fn remap_model_name_for_api(&self) -> String {
        remap_model_name_for_api(&self.provider, &self.model_name)
    }

    /// Generic OpenAI-compatible API call with optional bearer auth.
    async fn call_openai_compatible_with_optional_auth(
        &self,
        request: CompletionRequest,
        provider_display_name: &str,
        endpoint: &str,
        api_key: Option<String>,
        extra_headers: &[(&str, &str)],
    ) -> Result<completion::CompletionResponse<RawResponse>, CompletionError> {
        let stream = self
            .stream_openai_compatible_with_optional_auth(
                request,
                provider_display_name,
                endpoint,
                api_key,
                extra_headers,
            )
            .await?;
        collect_streaming_completion_response(stream).await
    }

    async fn stream_openai_compatible_with_optional_auth(
        &self,
        request: CompletionRequest,
        provider_display_name: &str,
        endpoint: &str,
        api_key: Option<String>,
        extra_headers: &[(&str, &str)],
    ) -> Result<StreamingCompletionResponse<RawStreamingResponse>, CompletionError> {
        let mut messages = Vec::new();

        if let Some(preamble) = &request.preamble {
            messages.push(serde_json::json!({
                "role": "system",
                "content": preamble,
            }));
        }

        messages.extend(convert_messages_to_openai(&request.chat_history));

        let api_model_name = self.remap_model_name_for_api();
        let mut body = serde_json::json!({
            "model": api_model_name,
            "messages": messages,
        });

        if let Some(max_tokens) = request.max_tokens {
            body["max_tokens"] = serde_json::json!(max_tokens);
        }

        if let Some(temperature) = request.temperature {
            body["temperature"] = serde_json::json!(temperature);
        }

        if !request.tools.is_empty() {
            let tools: Vec<serde_json::Value> = request
                .tools
                .iter()
                .map(|t| {
                    serde_json::json!({
                        "type": "function",
                        "function": {
                            "name": t.name,
                            "description": t.description,
                            "parameters": t.parameters,
                        }
                    })
                })
                .collect();
            body["tools"] = serde_json::json!(tools);
        }

        let http_client = self.llm_manager.http_client().clone();
        let endpoint = endpoint.to_string();
        let auth_header = api_key.map(|key| format!("Bearer {key}"));
        let extra_headers: Vec<(String, String)> = extra_headers
            .iter()
            .map(|(name, value)| ((*name).to_string(), (*value).to_string()))
            .collect();

        self.stream_openai_chat_request(
            move |request_body| {
                let mut request_builder = http_client.post(&endpoint);

                for (header_name, header_value) in &extra_headers {
                    request_builder = request_builder.header(header_name, header_value);
                }

                if let Some(auth_header) = auth_header.as_deref() {
                    request_builder = request_builder.header("authorization", auth_header);
                }

                request_builder
                    .header("content-type", "application/json")
                    .json(request_body)
            },
            body,
            provider_display_name,
        )
        .await
    }

    async fn stream_openai_chat_request<F>(
        &self,
        mut build_request: F,
        request_body: serde_json::Value,
        provider_label: &str,
    ) -> Result<StreamingCompletionResponse<RawStreamingResponse>, CompletionError>
    where
        F: FnMut(&serde_json::Value) -> reqwest::RequestBuilder,
    {
        let stream_request_body = with_streaming_enabled(&request_body);
        let response = build_request(&stream_request_body)
            .header("accept-encoding", "identity")
            .timeout(std::time::Duration::from_secs(STREAM_REQUEST_TIMEOUT_SECS))
            .send()
            .await
            .map_err(|error| CompletionError::ProviderError(error.to_string()))?;

        let status = response.status();
        if !status.is_success() {
            let response_text = response
                .text()
                .await
                .unwrap_or_else(|error| format!("failed to read error response body: {error}"));

            return Err(CompletionError::ProviderError(format!(
                "{provider_label} API error ({})",
                format_api_error_from_response_text(status, &response_text)
            )));
        }

        let provider_label = provider_label.to_string();
        let stream = async_stream::stream! {
            let mut stream = response.bytes_stream();
            let mut block_buffer = String::new();
            let mut raw_text = String::new();
            let mut sse_text = String::new();
            let mut saw_data_event = false;
            let mut pending_tool_calls: BTreeMap<usize, OpenAiStreamingToolCall> = BTreeMap::new();

            while let Some(chunk_result) = stream.next().await {
                let chunk = match chunk_result {
                    Ok(bytes) => bytes,
                    Err(error) => {
                        yield Err(CompletionError::ProviderError(format!(
                            "{provider_label} stream read failed: {error}"
                        )));
                        return;
                    }
                };

                let chunk_text = String::from_utf8_lossy(&chunk).to_string();
                if !saw_data_event {
                    raw_text.push_str(&chunk_text);
                }
                block_buffer.push_str(&chunk_text);

                while let Some(block) = extract_sse_block(&mut block_buffer) {
                    sse_text.push_str(&block);
                    sse_text.push_str("\n\n");

                    let Some(data) = extract_sse_data_payload(&block) else {
                        continue;
                    };
                    let data = data.trim();
                    if data.is_empty() || data == "[DONE]" {
                        continue;
                    }

                    saw_data_event = true;

                    let event_body = match serde_json::from_str::<serde_json::Value>(data) {
                        Ok(body) => body,
                        Err(error) => {
                            tracing::trace!(%error, payload = %data, "failed to parse OpenAI SSE chunk");
                            continue;
                        }
                    };

                    match process_openai_chat_stream_event(&event_body, &mut pending_tool_calls) {
                        Ok(events) => {
                            for event in events {
                                yield Ok(event);
                            }
                        }
                        Err(error) => {
                            yield Err(error);
                            return;
                        }
                    }
                }
            }

            if !block_buffer.trim().is_empty()
                && let Some(data) = extract_sse_data_payload(&block_buffer)
            {
                let data = data.trim();
                if !data.is_empty() && data != "[DONE]" {
                    saw_data_event = true;
                    if let Ok(event_body) = serde_json::from_str::<serde_json::Value>(data) {
                        match process_openai_chat_stream_event(&event_body, &mut pending_tool_calls) {
                            Ok(events) => {
                                for event in events {
                                    yield Ok(event);
                                }
                            }
                            Err(error) => {
                                yield Err(error);
                                return;
                            }
                        }
                    }
                }
            }

            match flush_openai_streaming_tool_calls(&mut pending_tool_calls) {
                Ok(events) => {
                    for event in events {
                        yield Ok(event);
                    }
                }
                Err(error) => {
                    yield Err(error);
                    return;
                }
            }

            if saw_data_event {
                let response_body = match parse_openai_chat_sse_response(&sse_text, &provider_label) {
                    Ok(body) => body,
                    Err(error) => {
                        yield Err(error);
                        return;
                    }
                };

                let parsed_response = match parse_openai_response(response_body.clone(), &provider_label) {
                    Ok(response) => response,
                    Err(error) => {
                        yield Err(error);
                        return;
                    }
                };

                yield Ok(RawStreamingChoice::FinalResponse(RawStreamingResponse {
                    body: response_body,
                    usage: Some(parsed_response.usage),
                }));
                return;
            }

            let response_body = match serde_json::from_str::<serde_json::Value>(&raw_text) {
                Ok(body) => body,
                Err(error) => {
                    yield Err(CompletionError::ProviderError(format!(
                        "{provider_label} response is neither SSE nor JSON: {error}. Body: {}",
                        truncate_body(&raw_text)
                    )));
                    return;
                }
            };

            let parsed_response = match parse_openai_response(response_body.clone(), &provider_label) {
                Ok(response) => response,
                Err(error) => {
                    yield Err(error);
                    return;
                }
            };

            for event in completion_choice_to_streaming_choices(&parsed_response.choice) {
                yield Ok(event);
            }
            if let Some(message_id) = parsed_response.message_id {
                yield Ok(RawStreamingChoice::MessageId(message_id));
            }

            yield Ok(RawStreamingChoice::FinalResponse(RawStreamingResponse {
                body: response_body,
                usage: Some(parsed_response.usage),
            }));
        };

        Ok(StreamingCompletionResponse::stream(Box::pin(stream)))
    }
}
// --- Helpers ---

/// Reverse-map Claude Code canonical tool names back to the original names
/// from the request's tool definitions.
fn reverse_map_tool_names(
    completion: &mut completion::CompletionResponse<RawResponse>,
    original_tools: &[(String, String)],
) {
    for content in completion.choice.iter_mut() {
        if let AssistantContent::ToolCall(tc) = content {
            tc.function.name =
                crate::llm::anthropic::from_claude_code_name(&tc.function.name, original_tools);
        }
    }
}

fn tool_result_content_to_string(content: &OneOrMany<rig::message::ToolResultContent>) -> String {
    content
        .iter()
        .filter_map(|c| match c {
            rig::message::ToolResultContent::Text(t) => Some(t.text.clone()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

// --- Message conversion ---

pub fn convert_messages_to_anthropic(messages: &OneOrMany<Message>) -> Vec<serde_json::Value> {
    messages
        .iter()
        .filter_map(|message| match message {
            Message::User { content } => {
                let parts: Vec<serde_json::Value> = content
                    .iter()
                    .filter_map(|c| match c {
                        UserContent::Text(t) => (!t.text.trim().is_empty())
                            .then(|| serde_json::json!({"type": "text", "text": t.text})),
                        UserContent::Image(image) => convert_image_anthropic(image),
                        UserContent::ToolResult(result) => Some(serde_json::json!({
                            "type": "tool_result",
                            "tool_use_id": result.id,
                            "content": tool_result_content_to_string(&result.content),
                        })),
                        _ => None,
                    })
                    .collect();
                (!parts.is_empty()).then(|| serde_json::json!({"role": "user", "content": parts}))
            }
            Message::Assistant { content, .. } => {
                let parts: Vec<serde_json::Value> = content
                    .iter()
                    .filter_map(|c| match c {
                        AssistantContent::Text(t) => (!t.text.trim().is_empty())
                            .then(|| serde_json::json!({"type": "text", "text": t.text})),
                        AssistantContent::ToolCall(tc) => Some(serde_json::json!({
                            "type": "tool_use",
                            "id": tc.id,
                            "name": tc.function.name,
                            "input": tc.function.arguments,
                        })),
                        _ => None,
                    })
                    .collect();
                (!parts.is_empty())
                    .then(|| serde_json::json!({"role": "assistant", "content": parts}))
            }
        })
        .collect()
}

fn convert_messages_to_openai(messages: &OneOrMany<Message>) -> Vec<serde_json::Value> {
    let mut result = Vec::new();

    for message in messages.iter() {
        match message {
            Message::User { content } => {
                // Separate tool results (they need their own messages) from content parts
                let mut content_parts: Vec<serde_json::Value> = Vec::new();
                let mut tool_results: Vec<serde_json::Value> = Vec::new();

                for item in content.iter() {
                    match item {
                        UserContent::Text(t) => {
                            content_parts.push(serde_json::json!({
                                "type": "text",
                                "text": t.text,
                            }));
                        }
                        UserContent::Image(image) => {
                            if let Some(part) = convert_image_openai(image) {
                                content_parts.push(part);
                            }
                        }
                        UserContent::ToolResult(tr) => {
                            let tool_call_id = tr
                                .call_id
                                .as_deref()
                                .filter(|call_id| !call_id.is_empty())
                                .unwrap_or(&tr.id);
                            tool_results.push(serde_json::json!({
                                "role": "tool",
                                "tool_call_id": tool_call_id,
                                "content": tool_result_content_to_string(&tr.content),
                            }));
                        }
                        _ => {}
                    }
                }

                if !content_parts.is_empty() {
                    // If there's only one text part and no images, use simple string format
                    if content_parts.len() == 1 && content_parts[0]["type"] == "text" {
                        result.push(serde_json::json!({
                            "role": "user",
                            "content": content_parts[0]["text"],
                        }));
                    } else {
                        // Mixed content (text + images): use array-of-parts format
                        result.push(serde_json::json!({
                            "role": "user",
                            "content": content_parts,
                        }));
                    }
                }

                result.extend(tool_results);
            }
            Message::Assistant { content, .. } => {
                let mut text_parts = Vec::new();
                let mut tool_calls = Vec::new();

                for item in content.iter() {
                    match item {
                        AssistantContent::Text(t) => {
                            text_parts.push(t.text.clone());
                        }
                        AssistantContent::ToolCall(tc) => {
                            // OpenAI expects arguments as a JSON string.
                            // Prefer call_id (set when replaying Responses-API tool calls
                            // through chat-completions) to keep assistant and tool IDs aligned.
                            let preferred_id = tc
                                .call_id
                                .as_deref()
                                .filter(|c| !c.is_empty())
                                .unwrap_or(&tc.id);
                            let args_string = serde_json::to_string(&tc.function.arguments)
                                .unwrap_or_else(|_| "{}".to_string());
                            tool_calls.push(serde_json::json!({
                                "id": preferred_id,
                                "type": "function",
                                "function": {
                                    "name": tc.function.name,
                                    "arguments": args_string,
                                }
                            }));
                        }
                        _ => {}
                    }
                }

                let mut msg = serde_json::json!({"role": "assistant"});
                if !text_parts.is_empty() {
                    msg["content"] = serde_json::json!(text_parts.join("\n"));
                }
                if !tool_calls.is_empty() {
                    msg["tool_calls"] = serde_json::json!(tool_calls);
                }
                result.push(msg);
            }
        }
    }

    result
}

fn convert_messages_to_openai_responses(messages: &OneOrMany<Message>) -> Vec<serde_json::Value> {
    let mut result = Vec::new();

    for message in messages.iter() {
        match message {
            Message::User { content } => {
                let mut content_parts = Vec::new();

                for item in content.iter() {
                    match item {
                        UserContent::Text(text) => {
                            content_parts.push(serde_json::json!({
                                "type": "input_text",
                                "text": text.text,
                            }));
                        }
                        UserContent::Image(image) => {
                            if let Some(part) = convert_image_openai_responses(image) {
                                content_parts.push(part);
                            }
                        }
                        UserContent::ToolResult(tool_result) => {
                            let call_id = tool_result
                                .call_id
                                .as_deref()
                                .filter(|call_id| !call_id.is_empty())
                                .unwrap_or(&tool_result.id);
                            result.push(serde_json::json!({
                                "type": "function_call_output",
                                "call_id": call_id,
                                "output": tool_result_content_to_string(&tool_result.content),
                            }));
                        }
                        _ => {}
                    }
                }

                if !content_parts.is_empty() {
                    result.push(serde_json::json!({
                        "role": "user",
                        "content": content_parts,
                    }));
                }
            }
            Message::Assistant { content, .. } => {
                let mut text_parts = Vec::new();

                for item in content.iter() {
                    match item {
                        AssistantContent::Text(text) => {
                            text_parts.push(serde_json::json!({
                                "type": "output_text",
                                "text": text.text,
                            }));
                        }
                        AssistantContent::ToolCall(tool_call) => {
                            let arguments = serde_json::to_string(&tool_call.function.arguments)
                                .unwrap_or_else(|_| "{}".to_string());
                            let call_id = tool_call
                                .call_id
                                .as_deref()
                                .filter(|call_id| !call_id.is_empty())
                                .unwrap_or(&tool_call.id);
                            result.push(serde_json::json!({
                                "type": "function_call",
                                "name": tool_call.function.name,
                                "arguments": arguments,
                                "call_id": call_id,
                            }));
                        }
                        _ => {}
                    }
                }

                if !text_parts.is_empty() {
                    result.push(serde_json::json!({
                        "role": "assistant",
                        "content": text_parts,
                    }));
                }
            }
        }
    }

    result
}

// --- Image conversion helpers ---

/// Convert a rig Image to an Anthropic image content block.
/// Anthropic format: {"type": "image", "source": {"type": "base64", "media_type": "image/jpeg", "data": "..."}}
fn convert_image_anthropic(image: &Image) -> Option<serde_json::Value> {
    let media_type = image
        .media_type
        .as_ref()
        .map(|mt| mt.to_mime_type())
        .unwrap_or("image/jpeg");

    match &image.data {
        DocumentSourceKind::Base64(data) => Some(serde_json::json!({
            "type": "image",
            "source": {
                "type": "base64",
                "media_type": media_type,
                "data": data,
            }
        })),
        DocumentSourceKind::Url(url) => Some(serde_json::json!({
            "type": "image",
            "source": {
                "type": "url",
                "url": url,
            }
        })),
        _ => None,
    }
}

/// Convert a rig Image to an OpenAI image_url content part.
/// OpenAI/OpenRouter format: {"type": "image_url", "image_url": {"url": "data:image/jpeg;base64,..."}}
fn convert_image_openai(image: &Image) -> Option<serde_json::Value> {
    let media_type = image
        .media_type
        .as_ref()
        .map(|mt| mt.to_mime_type())
        .unwrap_or("image/jpeg");

    match &image.data {
        DocumentSourceKind::Base64(data) => {
            let data_url = format!("data:{media_type};base64,{data}");
            Some(serde_json::json!({
                "type": "image_url",
                "image_url": { "url": data_url }
            }))
        }
        DocumentSourceKind::Url(url) => Some(serde_json::json!({
            "type": "image_url",
            "image_url": { "url": url }
        })),
        _ => None,
    }
}

fn convert_image_openai_responses(image: &Image) -> Option<serde_json::Value> {
    let media_type = image
        .media_type
        .as_ref()
        .map(|mime_type| mime_type.to_mime_type())
        .unwrap_or("image/jpeg");

    match &image.data {
        DocumentSourceKind::Base64(data) => {
            let data_url = format!("data:{media_type};base64,{data}");
            Some(serde_json::json!({
                "type": "input_image",
                "image_url": data_url,
            }))
        }
        DocumentSourceKind::Url(url) => Some(serde_json::json!({
            "type": "input_image",
            "image_url": url,
        })),
        _ => None,
    }
}

/// Truncate a response body for error messages to avoid dumping megabytes of HTML.
fn truncate_body(body: &str) -> &str {
    let limit = 500;
    if body.len() <= limit {
        body
    } else {
        &body[..limit]
    }
}

fn with_streaming_enabled(request_body: &serde_json::Value) -> serde_json::Value {
    let mut body = request_body.clone();
    body["stream"] = serde_json::json!(true);
    body
}

fn format_api_error_from_response_text(status: reqwest::StatusCode, response_text: &str) -> String {
    if let Ok(body) = serde_json::from_str::<serde_json::Value>(response_text) {
        format_api_error(status, &body)
    } else if let Some(message) = parse_openai_error_message(response_text) {
        format!("{status}: {message}")
    } else {
        format!("{status}: {}", truncate_body(response_text))
    }
}

struct OpenAiStreamingToolCall {
    id: String,
    internal_call_id: String,
    name: String,
    arguments: String,
}

impl Default for OpenAiStreamingToolCall {
    fn default() -> Self {
        Self {
            id: String::new(),
            internal_call_id: uuid::Uuid::new_v4().to_string(),
            name: String::new(),
            arguments: String::new(),
        }
    }
}

fn stream_from_completion_response(
    response: completion::CompletionResponse<RawResponse>,
) -> StreamingCompletionResponse<RawStreamingResponse> {
    let usage = response.usage;
    let message_id = response.message_id;
    let raw_body = response.raw_response.body;
    let choice_items: Vec<AssistantContent> = response.choice.into_iter().collect();

    let stream = async_stream::stream! {
        if let Some(message_id) = message_id {
            yield Ok(RawStreamingChoice::MessageId(message_id));
        }

        for content in choice_items {
            match content {
                AssistantContent::Text(text) => {
                    if !text.text.is_empty() {
                        yield Ok(RawStreamingChoice::Message(text.text));
                    }
                }
                AssistantContent::ToolCall(tool_call) => {
                    yield Ok(RawStreamingChoice::ToolCall(RawStreamingToolCall {
                        id: tool_call.id.clone(),
                        internal_call_id: if tool_call.id.is_empty() {
                            uuid::Uuid::new_v4().to_string()
                        } else {
                            tool_call.id
                        },
                        call_id: tool_call.call_id,
                        name: tool_call.function.name,
                        arguments: tool_call.function.arguments,
                        signature: tool_call.signature,
                        additional_params: tool_call.additional_params,
                    }));
                }
                AssistantContent::Reasoning(reasoning) => {
                    let reasoning_id = reasoning.id.clone();
                    for content in reasoning.content {
                        yield Ok(RawStreamingChoice::Reasoning {
                            id: reasoning_id.clone(),
                            content,
                        });
                    }
                }
                _ => {}
            }
        }

        yield Ok(RawStreamingChoice::FinalResponse(RawStreamingResponse {
            body: raw_body,
            usage: Some(usage),
        }));
    };

    StreamingCompletionResponse::stream(Box::pin(stream))
}

async fn collect_streaming_completion_response(
    mut stream: StreamingCompletionResponse<RawStreamingResponse>,
) -> Result<completion::CompletionResponse<RawResponse>, CompletionError> {
    while let Some(chunk) = stream.next().await {
        chunk?;
    }

    let raw_response = stream.response.unwrap_or(RawStreamingResponse {
        body: serde_json::json!({}),
        usage: None,
    });

    Ok(completion::CompletionResponse {
        choice: stream.choice,
        usage: raw_response.usage.unwrap_or_default(),
        raw_response: RawResponse {
            body: raw_response.body,
        },
        message_id: stream.message_id,
    })
}

fn completion_choice_to_streaming_choices(
    choice: &OneOrMany<AssistantContent>,
) -> Vec<RawStreamingChoice<RawStreamingResponse>> {
    let mut events = Vec::new();

    for content in choice.iter() {
        match content {
            AssistantContent::Text(text) => {
                if !text.text.is_empty() {
                    events.push(RawStreamingChoice::Message(text.text.clone()));
                }
            }
            AssistantContent::ToolCall(tool_call) => {
                events.push(RawStreamingChoice::ToolCall(RawStreamingToolCall {
                    id: tool_call.id.clone(),
                    internal_call_id: if tool_call.id.is_empty() {
                        uuid::Uuid::new_v4().to_string()
                    } else {
                        tool_call.id.clone()
                    },
                    call_id: tool_call.call_id.clone(),
                    name: tool_call.function.name.clone(),
                    arguments: tool_call.function.arguments.clone(),
                    signature: tool_call.signature.clone(),
                    additional_params: tool_call.additional_params.clone(),
                }));
            }
            AssistantContent::Reasoning(reasoning) => {
                for content in &reasoning.content {
                    events.push(RawStreamingChoice::Reasoning {
                        id: reasoning.id.clone(),
                        content: content.clone(),
                    });
                }
            }
            _ => {}
        }
    }

    events
}

fn extract_sse_block(buffer: &mut String) -> Option<String> {
    let (block_end, separator_len) = if let Some(index) = buffer.find("\n\n") {
        (index, 2)
    } else if let Some(index) = buffer.find("\r\n\r\n") {
        (index, 4)
    } else {
        return None;
    };

    let block = buffer[..block_end].to_string();
    *buffer = buffer[block_end + separator_len..].to_string();
    Some(block)
}

fn extract_sse_data_payload(block: &str) -> Option<String> {
    let mut data_lines = Vec::new();

    for line in block.lines() {
        if let Some(data) = line.strip_prefix("data: ") {
            data_lines.push(data);
        } else if let Some(data) = line.strip_prefix("data:") {
            data_lines.push(data.trim_start());
        }
    }

    if data_lines.is_empty() {
        None
    } else {
        Some(data_lines.join("\n"))
    }
}

fn escape_control_characters_in_json_strings(input: &str) -> String {
    let mut escaped = String::with_capacity(input.len());
    let mut in_string = false;
    let mut escape_next = false;

    for character in input.chars() {
        if in_string {
            if escape_next {
                escaped.push(character);
                escape_next = false;
                continue;
            }

            match character {
                '\\' => {
                    escaped.push(character);
                    escape_next = true;
                }
                '"' => {
                    escaped.push(character);
                    in_string = false;
                }
                '\u{0008}' => escaped.push_str("\\b"),
                '\t' => escaped.push_str("\\t"),
                '\n' => escaped.push_str("\\n"),
                '\u{000c}' => escaped.push_str("\\f"),
                '\r' => escaped.push_str("\\r"),
                '\u{0000}'..='\u{001f}' => {
                    let codepoint = character as u32;
                    escaped.push_str(&format!("\\u{codepoint:04x}"));
                }
                _ => escaped.push(character),
            }
            continue;
        }

        if character == '"' {
            in_string = true;
        }
        escaped.push(character);
    }

    escaped
}

fn parse_streamed_tool_arguments(
    tool_name: &str,
    raw_arguments: &str,
) -> Result<serde_json::Value, CompletionError> {
    if raw_arguments.trim().is_empty() {
        return Ok(serde_json::json!({}));
    }

    let direct_parse_error = match serde_json::from_str::<serde_json::Value>(raw_arguments) {
        Ok(arguments) => return Ok(arguments),
        Err(error) => error,
    };

    let sanitized_arguments = escape_control_characters_in_json_strings(raw_arguments);
    if sanitized_arguments != raw_arguments {
        match serde_json::from_str::<serde_json::Value>(&sanitized_arguments) {
            Ok(arguments) => {
                tracing::warn!(
                    tool_name,
                    "normalized control characters in streamed tool arguments"
                );
                return Ok(arguments);
            }
            Err(sanitized_parse_error) => {
                return Err(CompletionError::ProviderError(format!(
                    "invalid streamed tool arguments for '{tool_name}': {direct_parse_error}; after sanitization: {sanitized_parse_error}"
                )));
            }
        }
    }

    Err(CompletionError::ProviderError(format!(
        "invalid streamed tool arguments for '{tool_name}': {direct_parse_error}"
    )))
}

fn flush_openai_streaming_tool_calls(
    pending_tool_calls: &mut BTreeMap<usize, OpenAiStreamingToolCall>,
) -> Result<Vec<RawStreamingChoice<RawStreamingResponse>>, CompletionError> {
    let mut flushed = Vec::new();

    for (index, tool_call) in std::mem::take(pending_tool_calls) {
        if tool_call.name.trim().is_empty() {
            continue;
        }

        let id = if tool_call.id.is_empty() {
            format!("tool_call_{index}")
        } else {
            tool_call.id
        };

        let arguments = parse_streamed_tool_arguments(&tool_call.name, &tool_call.arguments)?;

        flushed.push(RawStreamingChoice::ToolCall(RawStreamingToolCall {
            id,
            internal_call_id: tool_call.internal_call_id,
            call_id: None,
            name: tool_call.name,
            arguments,
            signature: None,
            additional_params: None,
        }));
    }

    Ok(flushed)
}

fn process_openai_chat_stream_event(
    event_body: &serde_json::Value,
    pending_tool_calls: &mut BTreeMap<usize, OpenAiStreamingToolCall>,
) -> Result<Vec<RawStreamingChoice<RawStreamingResponse>>, CompletionError> {
    if let Some(error_body) = event_body.get("error") {
        let message = error_body["message"].as_str().unwrap_or("unknown error");
        return Err(CompletionError::ProviderError(format!(
            "OpenAI-compatible streaming error: {message}"
        )));
    }

    let mut events = Vec::new();
    let Some(choices) = event_body
        .get("choices")
        .and_then(serde_json::Value::as_array)
    else {
        return Ok(events);
    };

    for choice in choices {
        if let Some(delta) = choice.get("delta") {
            if let Some(content) = delta.get("content") {
                let mut text_parts = Vec::new();
                collect_openai_text_content(content, &mut text_parts);
                for text in text_parts {
                    events.push(RawStreamingChoice::Message(text));
                }
            }

            if let Some(reasoning) = delta.get("reasoning") {
                let mut reasoning_parts = Vec::new();
                collect_openai_text_content(reasoning, &mut reasoning_parts);
                for reasoning in reasoning_parts {
                    events.push(RawStreamingChoice::ReasoningDelta {
                        id: None,
                        reasoning,
                    });
                }
            }

            if let Some(reasoning_content) = delta.get("reasoning_content") {
                let mut reasoning_parts = Vec::new();
                collect_openai_text_content(reasoning_content, &mut reasoning_parts);
                for reasoning in reasoning_parts {
                    events.push(RawStreamingChoice::ReasoningDelta {
                        id: None,
                        reasoning,
                    });
                }
            }

            if let Some(delta_tool_calls) = delta
                .get("tool_calls")
                .and_then(serde_json::Value::as_array)
            {
                for tool_call in delta_tool_calls {
                    let index = tool_call
                        .get("index")
                        .and_then(serde_json::Value::as_u64)
                        .unwrap_or(0) as usize;
                    let entry = pending_tool_calls.entry(index).or_default();

                    if let Some(id) = tool_call.get("id").and_then(serde_json::Value::as_str)
                        && !id.is_empty()
                    {
                        entry.id = id.to_string();
                    }

                    let function = tool_call
                        .get("function")
                        .unwrap_or(&serde_json::Value::Null);
                    if let Some(name) = function.get("name").and_then(serde_json::Value::as_str)
                        && !name.is_empty()
                    {
                        entry.name = name.to_string();
                        events.push(RawStreamingChoice::ToolCallDelta {
                            id: entry.id.clone(),
                            internal_call_id: entry.internal_call_id.clone(),
                            content: rig::streaming::ToolCallDeltaContent::Name(name.to_string()),
                        });
                    }

                    if let Some(arguments_chunk) = function
                        .get("arguments")
                        .and_then(serde_json::Value::as_str)
                        && !arguments_chunk.is_empty()
                    {
                        entry.arguments.push_str(arguments_chunk);
                        events.push(RawStreamingChoice::ToolCallDelta {
                            id: entry.id.clone(),
                            internal_call_id: entry.internal_call_id.clone(),
                            content: rig::streaming::ToolCallDeltaContent::Delta(
                                arguments_chunk.to_string(),
                            ),
                        });
                    }
                }
            }
        }

        if let Some(message) = choice.get("message") {
            if let Some(content) = message.get("content") {
                let mut text_parts = Vec::new();
                collect_openai_text_content(content, &mut text_parts);
                for text in text_parts {
                    events.push(RawStreamingChoice::Message(text));
                }
            }

            if let Some(message_tool_calls) = message
                .get("tool_calls")
                .and_then(serde_json::Value::as_array)
            {
                for (index, tool_call) in message_tool_calls.iter().enumerate() {
                    let pending_tool_call = pending_tool_calls.remove(&index);
                    let fallback_id = pending_tool_call
                        .as_ref()
                        .and_then(|pending| {
                            if pending.id.is_empty() {
                                None
                            } else {
                                Some(pending.id.clone())
                            }
                        })
                        .unwrap_or_else(|| format!("tool_call_{index}"));
                    if let Some(tool_call) = parse_openai_tool_call(tool_call, fallback_id) {
                        let internal_call_id = pending_tool_call
                            .as_ref()
                            .map(|pending| pending.internal_call_id.clone())
                            .unwrap_or_else(|| {
                                if tool_call.id.is_empty() {
                                    uuid::Uuid::new_v4().to_string()
                                } else {
                                    tool_call.id.clone()
                                }
                            });

                        events.push(RawStreamingChoice::ToolCall(RawStreamingToolCall {
                            id: tool_call.id.clone(),
                            internal_call_id,
                            call_id: tool_call.call_id,
                            name: tool_call.function.name,
                            arguments: tool_call.function.arguments,
                            signature: tool_call.signature,
                            additional_params: tool_call.additional_params,
                        }));
                    }
                }
            }
        }

        if let Some(finish_reason) = choice
            .get("finish_reason")
            .and_then(serde_json::Value::as_str)
            && matches!(finish_reason, "tool_calls" | "function_call")
        {
            events.extend(flush_openai_streaming_tool_calls(pending_tool_calls)?);
        }
    }

    Ok(events)
}

fn parse_openai_chat_sse_response(
    response_text: &str,
    provider_label: &str,
) -> Result<serde_json::Value, CompletionError> {
    let mut text_parts = Vec::new();
    let mut reasoning_parts = Vec::new();
    let mut tool_calls: BTreeMap<usize, OpenAiStreamingToolCall> = BTreeMap::new();
    let mut usage = None;
    let mut finish_reason = None;
    let mut saw_data_event = false;

    let mut process_payload = |payload: &str| -> Result<(), CompletionError> {
        let data = payload.trim();
        if data.is_empty() || data == "[DONE]" {
            return Ok(());
        }

        saw_data_event = true;

        let Ok(event_body) = serde_json::from_str::<serde_json::Value>(data) else {
            return Ok(());
        };

        if let Some(error_body) = event_body.get("error") {
            let message = error_body["message"].as_str().unwrap_or("unknown error");
            return Err(CompletionError::ProviderError(format!(
                "{provider_label} streaming error: {message}"
            )));
        }

        if let Some(chunk_usage) = event_body.get("usage")
            && !chunk_usage.is_null()
        {
            usage = Some(chunk_usage.clone());
        }

        let Some(choices) = event_body
            .get("choices")
            .and_then(serde_json::Value::as_array)
        else {
            return Ok(());
        };

        for choice in choices {
            if finish_reason.is_none()
                && let Some(reason) = choice
                    .get("finish_reason")
                    .and_then(serde_json::Value::as_str)
                && !reason.is_empty()
            {
                finish_reason = Some(reason.to_string());
            }

            if let Some(delta) = choice.get("delta") {
                if let Some(content) = delta.get("content") {
                    collect_openai_text_content(content, &mut text_parts);
                }
                if let Some(reasoning) = delta.get("reasoning") {
                    collect_openai_text_content(reasoning, &mut reasoning_parts);
                }
                if let Some(reasoning_content) = delta.get("reasoning_content") {
                    collect_openai_text_content(reasoning_content, &mut reasoning_parts);
                }

                if let Some(delta_tool_calls) = delta
                    .get("tool_calls")
                    .and_then(serde_json::Value::as_array)
                {
                    for tool_call in delta_tool_calls {
                        let index = tool_call
                            .get("index")
                            .and_then(serde_json::Value::as_u64)
                            .unwrap_or(0) as usize;
                        let entry = tool_calls.entry(index).or_default();

                        if let Some(id) = tool_call.get("id").and_then(serde_json::Value::as_str)
                            && !id.is_empty()
                        {
                            entry.id = id.to_string();
                        }

                        let function = tool_call
                            .get("function")
                            .unwrap_or(&serde_json::Value::Null);
                        if let Some(name) = function.get("name").and_then(serde_json::Value::as_str)
                            && !name.is_empty()
                        {
                            entry.name = name.to_string();
                        }

                        if let Some(arguments_chunk) = function
                            .get("arguments")
                            .and_then(serde_json::Value::as_str)
                            && !arguments_chunk.is_empty()
                        {
                            entry.arguments.push_str(arguments_chunk);
                        }
                    }
                }
            }

            if let Some(message) = choice.get("message") {
                if let Some(content) = message.get("content") {
                    collect_openai_text_content(content, &mut text_parts);
                }
                if let Some(reasoning) = message.get("reasoning") {
                    collect_openai_text_content(reasoning, &mut reasoning_parts);
                }
                if let Some(reasoning_content) = message.get("reasoning_content") {
                    collect_openai_text_content(reasoning_content, &mut reasoning_parts);
                }

                if let Some(message_tool_calls) = message
                    .get("tool_calls")
                    .and_then(serde_json::Value::as_array)
                {
                    for (index, tool_call) in message_tool_calls.iter().enumerate() {
                        let entry = tool_calls.entry(index).or_default();

                        if let Some(id) = tool_call.get("id").and_then(serde_json::Value::as_str)
                            && !id.is_empty()
                        {
                            entry.id = id.to_string();
                        }

                        let function = tool_call
                            .get("function")
                            .unwrap_or(&serde_json::Value::Null);
                        if let Some(name) = function.get("name").and_then(serde_json::Value::as_str)
                            && !name.is_empty()
                        {
                            entry.name = name.to_string();
                        }

                        if let Some(arguments) = function.get("arguments") {
                            if let Some(arguments_str) = arguments.as_str() {
                                entry.arguments = arguments_str.to_string();
                            } else {
                                entry.arguments = arguments.to_string();
                            }
                        }
                    }
                }
            }
        }

        Ok(())
    };

    let mut block_buffer = response_text.to_string();
    while let Some(block) = extract_sse_block(&mut block_buffer) {
        if let Some(payload) = extract_sse_data_payload(&block) {
            process_payload(&payload)?;
        }
    }
    if !block_buffer.trim().is_empty()
        && let Some(payload) = extract_sse_data_payload(&block_buffer)
    {
        process_payload(&payload)?;
    }

    if !saw_data_event {
        return Err(CompletionError::ProviderError(format!(
            "{provider_label} streaming response missing SSE data events. Body: {}",
            truncate_body(response_text)
        )));
    }

    let mut message = serde_json::json!({
        "content": if text_parts.is_empty() {
            serde_json::Value::Null
        } else {
            serde_json::Value::String(text_parts.join(""))
        }
    });

    if !reasoning_parts.is_empty() {
        message["reasoning"] = serde_json::Value::String(reasoning_parts.join(""));
    }

    let mut tool_call_values = Vec::new();
    for (index, tool_call) in tool_calls {
        if tool_call.name.trim().is_empty() {
            continue;
        }

        let id = if tool_call.id.is_empty() {
            format!("tool_call_{index}")
        } else {
            tool_call.id
        };
        let arguments =
            parse_openai_tool_arguments(&serde_json::Value::String(tool_call.arguments));

        tool_call_values.push(serde_json::json!({
            "id": id,
            "type": "function",
            "function": {
                "name": tool_call.name,
                "arguments": arguments,
            }
        }));
    }

    if !tool_call_values.is_empty() {
        message["tool_calls"] = serde_json::Value::Array(tool_call_values);
    }

    if message.get("tool_calls").is_none()
        && message["content"].is_null()
        && reasoning_parts.is_empty()
    {
        return Err(CompletionError::ProviderError(format!(
            "{provider_label} streaming response did not contain message content or tool calls. Body: {}",
            truncate_body(response_text)
        )));
    }

    let finish_reason = finish_reason.unwrap_or_else(|| {
        if message.get("tool_calls").is_some() {
            "tool_calls".to_string()
        } else {
            "stop".to_string()
        }
    });

    Ok(serde_json::json!({
        "choices": [{
            "message": message,
            "finish_reason": finish_reason,
        }],
        "usage": usage.unwrap_or_else(|| serde_json::json!({})),
    }))
}

// --- Response parsing ---

fn make_tool_call(id: String, name: String, arguments: serde_json::Value) -> ToolCall {
    ToolCall {
        id,
        call_id: None,
        function: ToolFunction {
            name: name.trim().to_string(),
            arguments,
        },
        signature: None,
        additional_params: None,
    }
}

fn parse_anthropic_response(
    body: serde_json::Value,
) -> Result<completion::CompletionResponse<RawResponse>, CompletionError> {
    let content_blocks = body["content"]
        .as_array()
        .ok_or_else(|| CompletionError::ResponseError("missing content array".into()))?;

    let mut assistant_content = Vec::new();

    for block in content_blocks {
        match block["type"].as_str() {
            Some("text") => {
                let text = block["text"].as_str().unwrap_or("");
                if !text.trim().is_empty() {
                    assistant_content.push(AssistantContent::Text(Text {
                        text: text.to_string(),
                    }));
                } else {
                    tracing::debug!("dropping empty text block in Anthropic response");
                }
            }
            Some("tool_use") => {
                let id = block["id"].as_str().unwrap_or("").to_string();
                let name = block["name"].as_str().unwrap_or("").to_string();
                let arguments = block["input"].clone();
                assistant_content.push(AssistantContent::ToolCall(make_tool_call(
                    id, name, arguments,
                )));
            }
            Some("thinking") => {
                // Thinking blocks contain internal reasoning, not actionable output.
                // We'll skip them but log for debugging.
                tracing::debug!("skipping thinking block in Anthropic response");
            }
            _ => {
                // Unknown block type - log but skip
                tracing::debug!(
                    "skipping unknown block type in Anthropic response: {:?}",
                    block["type"].as_str()
                );
            }
        }
    }

    let choice = match OneOrMany::many(assistant_content) {
        Ok(choice) => choice,
        Err(_) => {
            // Anthropic can return an empty content array when stop_reason is
            // end_turn and the model has nothing further to say (e.g. after a
            // side-effect-only tool call like react/skip). Treat this as a clean
            // no-op so the agentic loop terminates gracefully.
            let stop_reason = body["stop_reason"].as_str().unwrap_or("unknown");
            if stop_reason == "end_turn" {
                tracing::debug!(
                    stop_reason,
                    content_blocks = content_blocks.len(),
                    "empty assistant_content from Anthropic end_turn — returning synthetic whitespace placeholder"
                );
                OneOrMany::one(AssistantContent::Text(Text {
                    text: " ".to_string(),
                }))
            } else {
                tracing::warn!(
                    stop_reason,
                    content_blocks = content_blocks.len(),
                    "unexpected empty assistant_content from Anthropic"
                );
                return Err(CompletionError::ResponseError(format!(
                    "empty response from Anthropic (stop_reason: {stop_reason})"
                )));
            }
        }
    };

    let input_tokens = body["usage"]["input_tokens"].as_u64().unwrap_or(0);
    let output_tokens = body["usage"]["output_tokens"].as_u64().unwrap_or(0);
    let cached = body["usage"]["cache_read_input_tokens"]
        .as_u64()
        .unwrap_or(0);

    Ok(completion::CompletionResponse {
        choice,
        usage: completion::Usage {
            input_tokens,
            output_tokens,
            total_tokens: input_tokens + output_tokens,
            cached_input_tokens: cached,
        },
        raw_response: RawResponse { body },
        message_id: None,
    })
}

fn parse_openai_response(
    body: serde_json::Value,
    provider_label: &str,
) -> Result<completion::CompletionResponse<RawResponse>, CompletionError> {
    let choice = body["choices"]
        .as_array()
        .and_then(|choices| choices.first())
        .ok_or_else(|| CompletionError::ResponseError("missing choices array".into()))?;
    let message = choice.get("message").unwrap_or(&serde_json::Value::Null);

    let mut assistant_content = Vec::new();

    if let Some(content) = message.get("content") {
        let mut text_parts = Vec::new();
        collect_openai_text_content(content, &mut text_parts);
        for text in text_parts {
            assistant_content.push(AssistantContent::Text(Text { text }));
        }
    }

    if let Some(tool_calls) = message
        .get("tool_calls")
        .and_then(serde_json::Value::as_array)
    {
        for (index, tool_call_value) in tool_calls.iter().enumerate() {
            if let Some(tool_call) =
                parse_openai_tool_call(tool_call_value, format!("tool_call_{index}"))
            {
                assistant_content.push(AssistantContent::ToolCall(tool_call));
            }
        }
    }

    if let Some(function_call) = message.get("function_call")
        && let Some(tool_call) =
            parse_openai_tool_call(function_call, "function_call_0".to_string())
    {
        assistant_content.push(AssistantContent::ToolCall(tool_call));
    }

    // Some reasoning models (e.g., NVIDIA kimi-k2.5) return reasoning in a separate field
    if assistant_content.is_empty()
        && let Some(reasoning) = parse_openai_reasoning_fallback(message)
    {
        tracing::debug!(
            provider = %provider_label,
            "extracted reasoning fallback as main content"
        );
        assistant_content.push(AssistantContent::Text(Text { text: reasoning }));
    }

    if assistant_content.is_empty()
        && let Some(text) = choice["text"].as_str()
        && !text.trim().is_empty()
    {
        assistant_content.push(AssistantContent::Text(Text {
            text: text.to_string(),
        }));
    }

    let result_choice = OneOrMany::many(assistant_content).map_err(|_| {
        let finish_reason = choice["finish_reason"].as_str().unwrap_or("unknown");
        let message_keys = message
            .as_object()
            .map(|keys| {
                let mut key_names: Vec<String> = keys.keys().cloned().collect();
                key_names.sort();
                key_names.join(",")
            })
            .unwrap_or_else(|| "non-object".to_string());
        tracing::warn!(
            provider = %provider_label,
            finish_reason,
            message_keys = %message_keys,
            choice = ?choice,
            "empty response from provider"
        );
        CompletionError::ResponseError(format!(
            "empty response from {provider_label} (finish_reason: {finish_reason})"
        ))
    })?;

    let input_tokens = body["usage"]["prompt_tokens"].as_u64().unwrap_or(0);
    let output_tokens = body["usage"]["completion_tokens"].as_u64().unwrap_or(0);
    let cached = body["usage"]["prompt_tokens_details"]["cached_tokens"]
        .as_u64()
        .unwrap_or(0);

    Ok(completion::CompletionResponse {
        choice: result_choice,
        usage: completion::Usage {
            input_tokens,
            output_tokens,
            total_tokens: input_tokens + output_tokens,
            cached_input_tokens: cached,
        },
        raw_response: RawResponse { body },
        message_id: None,
    })
}

fn parse_openai_reasoning_fallback(message: &serde_json::Value) -> Option<String> {
    let mut reasoning_parts = Vec::new();

    if let Some(reasoning_content) = message.get("reasoning_content") {
        collect_openai_text_content(reasoning_content, &mut reasoning_parts);
    }
    if let Some(reasoning) = message.get("reasoning") {
        collect_openai_text_content(reasoning, &mut reasoning_parts);
    }
    if let Some(reasoning_details) = message.get("reasoning_details") {
        collect_openai_text_content(reasoning_details, &mut reasoning_parts);
    }

    if reasoning_parts.is_empty() {
        None
    } else {
        Some(reasoning_parts.join("\n"))
    }
}

fn collect_openai_text_content(value: &serde_json::Value, text_parts: &mut Vec<String>) {
    match value {
        serde_json::Value::String(text) => {
            if !text.trim().is_empty() {
                text_parts.push(text.to_string());
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                collect_openai_text_content(item, text_parts);
            }
        }
        serde_json::Value::Object(map) => {
            if let Some(text) = map.get("text").and_then(serde_json::Value::as_str)
                && !text.trim().is_empty()
            {
                text_parts.push(text.to_string());
            }
            if let Some(summary) = map.get("summary").and_then(serde_json::Value::as_str)
                && !summary.trim().is_empty()
            {
                text_parts.push(summary.to_string());
            }
            if let Some(refusal) = map.get("refusal").and_then(serde_json::Value::as_str)
                && !refusal.trim().is_empty()
            {
                text_parts.push(refusal.to_string());
            }

            if let Some(content) = map.get("content") {
                collect_openai_text_content(content, text_parts);
            }
        }
        _ => {}
    }
}

fn parse_openai_tool_arguments(arguments_field: &serde_json::Value) -> serde_json::Value {
    if let Some(raw) = arguments_field.as_str() {
        return serde_json::from_str(raw).unwrap_or_else(|_| serde_json::json!({}));
    }
    if arguments_field.is_null() {
        serde_json::json!({})
    } else {
        arguments_field.clone()
    }
}

fn parse_openai_tool_call(tool_call: &serde_json::Value, fallback_id: String) -> Option<ToolCall> {
    let name = tool_call
        .get("function")
        .and_then(|function| function.get("name"))
        .and_then(serde_json::Value::as_str)
        .or_else(|| tool_call.get("name").and_then(serde_json::Value::as_str))
        .map(str::trim)
        .unwrap_or("");
    if name.is_empty() {
        return None;
    }

    let id = tool_call
        .get("id")
        .and_then(serde_json::Value::as_str)
        .or_else(|| tool_call.get("call_id").and_then(serde_json::Value::as_str))
        .filter(|id| !id.is_empty())
        .map(ToOwned::to_owned)
        .unwrap_or(fallback_id);

    let null = serde_json::Value::Null;
    let arguments_field = tool_call
        .get("function")
        .and_then(|function| function.get("arguments"))
        .or_else(|| tool_call.get("arguments"))
        .unwrap_or(&null);
    let arguments = parse_openai_tool_arguments(arguments_field);

    Some(make_tool_call(id, name.to_string(), arguments))
}

fn extract_text_content_from_responses_output_item(
    value: &serde_json::Value,
    text_parts: &mut Vec<String>,
) {
    match value {
        serde_json::Value::Array(items) => {
            for item in items {
                extract_text_content_from_responses_output_item(item, text_parts);
            }
        }
        serde_json::Value::Object(map) => {
            if matches!(
                map.get("type").and_then(serde_json::Value::as_str),
                Some("function_call") | Some("function_call_output")
            ) {
                return;
            }

            if let Some(text) = map.get("text").and_then(serde_json::Value::as_str)
                && !text.trim().is_empty()
            {
                text_parts.push(text.to_string());
            }
            if let Some(summary) = map.get("summary") {
                collect_openai_text_content(summary, text_parts);
            }
            if let Some(refusal) = map.get("refusal") {
                collect_openai_text_content(refusal, text_parts);
            }
            if let Some(content) = map.get("content") {
                extract_text_content_from_responses_output_item(content, text_parts);
            }
        }
        _ => {}
    }
}

fn make_openai_responses_tool_call(
    id: String,
    call_id: Option<String>,
    name: String,
    arguments: serde_json::Value,
) -> ToolCall {
    ToolCall {
        id,
        call_id,
        function: ToolFunction {
            name: name.trim().to_string(),
            arguments,
        },
        signature: None,
        additional_params: None,
    }
}

fn parse_openai_responses_response(
    body: serde_json::Value,
    provider_label: &str,
) -> Result<completion::CompletionResponse<RawResponse>, CompletionError> {
    let output_items = body["output"]
        .as_array()
        .ok_or_else(|| CompletionError::ResponseError("missing output array".into()))?;

    let mut assistant_content = Vec::new();
    let mut fallback_text_parts = Vec::new();

    for (index, output_item) in output_items.iter().enumerate() {
        match output_item["type"].as_str() {
            Some("message") => {
                if let Some(content_items) = output_item["content"].as_array() {
                    let mut message_output_text = Vec::new();
                    let mut message_fallback_text = Vec::new();

                    for content_item in content_items {
                        if content_item["type"].as_str() == Some("output_text")
                            && let Some(text) = content_item["text"].as_str()
                            && !text.is_empty()
                        {
                            message_output_text.push(text.to_string());
                        }

                        extract_text_content_from_responses_output_item(
                            content_item,
                            &mut message_fallback_text,
                        );
                    }

                    if message_output_text.is_empty() {
                        fallback_text_parts.extend(message_fallback_text);
                    } else {
                        for text in message_output_text {
                            assistant_content.push(AssistantContent::Text(Text { text }));
                        }
                    }
                }
            }
            Some("function_call") => {
                let call_id = output_item["call_id"]
                    .as_str()
                    .filter(|id| !id.is_empty())
                    .map(ToOwned::to_owned);
                let id = output_item["id"]
                    .as_str()
                    .filter(|id| !id.is_empty())
                    .map(ToOwned::to_owned)
                    .or_else(|| call_id.clone())
                    .unwrap_or_else(|| format!("function_call_{index}"));
                let name = output_item["name"].as_str().unwrap_or("").to_string();
                let arguments = parse_openai_tool_arguments(&output_item["arguments"]);

                assistant_content.push(AssistantContent::ToolCall(
                    make_openai_responses_tool_call(id, call_id, name, arguments),
                ));
            }
            _ => {
                extract_text_content_from_responses_output_item(
                    output_item,
                    &mut fallback_text_parts,
                );
            }
        }
    }

    let has_text = assistant_content
        .iter()
        .any(|content| matches!(content, AssistantContent::Text(_)));
    if !has_text {
        for text in fallback_text_parts {
            assistant_content.push(AssistantContent::Text(Text { text }));
        }
    }

    let choice = OneOrMany::many(assistant_content).map_err(|_| {
        let output_types = output_items
            .iter()
            .map(|item| item["type"].as_str().unwrap_or("<missing-type>"))
            .collect::<Vec<_>>()
            .join(", ");
        tracing::warn!(
            provider = %provider_label,
            output_items = output_items.len(),
            output_types = %output_types,
            "empty response from responses API"
        );
        CompletionError::ResponseError(format!(
            "empty or unsupported response from {provider_label} Responses API; expected text-bearing message content (output_text/text/summary/refusal/content) or function_call output items; received output types: {output_types}"
        ))
    })?;

    let input_tokens = body["usage"]["input_tokens"].as_u64().unwrap_or(0);
    let output_tokens = body["usage"]["output_tokens"].as_u64().unwrap_or(0);
    let cached = body["usage"]["input_tokens_details"]["cached_tokens"]
        .as_u64()
        .unwrap_or(0);

    Ok(completion::CompletionResponse {
        choice,
        usage: completion::Usage {
            input_tokens,
            output_tokens,
            total_tokens: input_tokens + output_tokens,
            cached_input_tokens: cached,
        },
        raw_response: RawResponse { body },
        message_id: None,
    })
}

fn parse_openai_responses_sse_response(
    response_text: &str,
    provider_label: &str,
) -> Result<serde_json::Value, CompletionError> {
    for line in response_text.lines() {
        let Some(data) = line.strip_prefix("data: ") else {
            continue;
        };

        if data.trim().is_empty() || data.trim() == "[DONE]" {
            continue;
        }

        let Ok(event_body) = serde_json::from_str::<serde_json::Value>(data) else {
            continue;
        };

        if event_body["type"].as_str() == Some("response.completed")
            && let Some(response) = event_body.get("response")
        {
            return Ok(response.clone());
        }
    }

    Err(CompletionError::ProviderError(format!(
        "{provider_label} Responses SSE stream missing response.completed event.\nBody: {}",
        truncate_body(response_text)
    )))
}

fn parse_openai_error_message(response_text: &str) -> Option<String> {
    let parsed = serde_json::from_str::<serde_json::Value>(response_text).ok()?;
    parsed["error"]["message"]
        .as_str()
        .or(parsed["detail"].as_str())
        .or(parsed["message"].as_str())
        .map(ToOwned::to_owned)
}

/// Build a detailed error string from an OpenAI-compatible error response.
///
/// OpenRouter (and potentially other proxies) return additional context in
/// `error.metadata` — e.g. `provider_name` and `raw` (the upstream error).
/// Including this in the error message helps users diagnose issues like
/// misconfigured presets or model-specific rejections (see issue #262).
fn format_api_error(status: reqwest::StatusCode, body: &serde_json::Value) -> String {
    let message = body["error"]["message"].as_str().unwrap_or("unknown error");

    let provider_name = body["error"]["metadata"]["provider_name"]
        .as_str()
        .filter(|s| !s.is_empty());
    let raw = match &body["error"]["metadata"]["raw"] {
        serde_json::Value::String(s) if !s.is_empty() => Some(s.clone()),
        serde_json::Value::Null => None,
        other => {
            let s = other.to_string();
            if s == "null" { None } else { Some(s) }
        }
    };

    match (provider_name, raw.as_deref()) {
        (Some(provider), Some(raw_err)) => {
            format!("{status}: {message} (upstream provider {provider}: {raw_err})")
        }
        (Some(provider), None) => {
            format!("{status}: {message} (upstream provider: {provider})")
        }
        (None, Some(raw_err)) => {
            format!("{status}: {message} ({raw_err})")
        }
        (None, None) => {
            format!("{status}: {message}")
        }
    }
}

fn provider_display_name(provider_id: &str) -> String {
    match provider_id {
        "openai" => "OpenAI".to_string(),
        "openai-chatgpt" => "OpenAI ChatGPT".to_string(),
        "openrouter" => "OpenRouter".to_string(),
        "kilo" => "Kilo Gateway".to_string(),
        "zhipu" => "Z.AI (GLM)".to_string(),
        "groq" => "Groq".to_string(),
        "together" => "Together".to_string(),
        "fireworks" => "Fireworks".to_string(),
        "deepseek" => "DeepSeek".to_string(),
        "xai" => "xAI".to_string(),
        "mistral" => "Mistral".to_string(),
        "gemini" => "Google Gemini".to_string(),
        "moonshot" => "Moonshot".to_string(),
        "nvidia" => "NVIDIA".to_string(),
        "opencode-zen" => "OpenCode Zen".to_string(),
        "opencode-go" => "OpenCode Go".to_string(),
        "zai-coding-plan" => "Z.AI Coding Plan".to_string(),
        _ => provider_id.to_string(),
    }
}

fn remap_model_name_for_api(provider: &str, model_name: &str) -> String {
    if provider == "zai-coding-plan" {
        // Coding Plan endpoint expects plain model ids (e.g. "glm-5").
        model_name
            .strip_prefix("zai/")
            .unwrap_or(model_name)
            .to_string()
    } else {
        model_name.to_string()
    }
}

fn openai_reasoning_effort(model_name: &str, configured_effort: &str) -> Option<&'static str> {
    if configured_effort == "auto" || !model_name.starts_with("gpt-5") {
        return None;
    }

    let is_pro_model = model_name.contains("-pro");
    match configured_effort {
        "max" => Some("xhigh"),
        "high" => Some("high"),
        "medium" => Some("medium"),
        "low" if is_pro_model => Some("medium"),
        "low" => Some("low"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rig::message::Message;
    use std::collections::BTreeMap;

    #[test]
    fn reverse_map_restores_original_tool_names() {
        let original_tools = vec![
            ("my_read".to_string(), "reads files".to_string()),
            ("my_bash".to_string(), "runs commands".to_string()),
        ];

        let mut completion = completion::CompletionResponse {
            choice: OneOrMany::one(AssistantContent::ToolCall(ToolCall {
                id: "tc1".into(),
                call_id: None,
                function: ToolFunction {
                    name: "My_Read".into(),
                    arguments: serde_json::json!({}),
                },
                signature: None,
                additional_params: None,
            })),
            usage: completion::Usage {
                input_tokens: 0,
                output_tokens: 0,
                total_tokens: 0,
                cached_input_tokens: 0,
            },
            raw_response: RawResponse {
                body: serde_json::json!({}),
            },
            message_id: None,
        };

        reverse_map_tool_names(&mut completion, &original_tools);

        let first = completion.choice.first_ref();
        if let AssistantContent::ToolCall(tc) = first {
            assert_eq!(tc.function.name, "my_read");
        } else {
            panic!("expected ToolCall");
        }
    }
    #[test]
    fn coding_plan_model_name_uses_plain_glm_id() {
        assert_eq!(
            remap_model_name_for_api("zai-coding-plan", "glm-5"),
            "glm-5"
        );
        assert_eq!(
            remap_model_name_for_api("zai-coding-plan", "zai/glm-5"),
            "glm-5"
        );
        assert_eq!(
            remap_model_name_for_api("openai", "gpt-4o-mini"),
            "gpt-4o-mini"
        );
        assert_eq!(remap_model_name_for_api("openai", "zai/glm-5"), "zai/glm-5");
    }

    #[test]
    fn openai_reasoning_effort_omits_auto_and_non_gpt5_models() {
        assert_eq!(openai_reasoning_effort("gpt-5.4", "auto"), None);
        assert_eq!(openai_reasoning_effort("gpt-4.1", "high"), None);
    }

    #[test]
    fn openai_reasoning_effort_maps_standard_gpt5_values() {
        assert_eq!(openai_reasoning_effort("gpt-5.4", "low"), Some("low"));
        assert_eq!(openai_reasoning_effort("gpt-5.4", "medium"), Some("medium"));
        assert_eq!(openai_reasoning_effort("gpt-5.4", "high"), Some("high"));
        assert_eq!(openai_reasoning_effort("gpt-5.4", "max"), Some("xhigh"));
    }

    #[test]
    fn openai_reasoning_effort_raises_low_floor_for_pro_models() {
        assert_eq!(
            openai_reasoning_effort("gpt-5.4-pro", "low"),
            Some("medium")
        );
        assert_eq!(openai_reasoning_effort("gpt-5.4-pro", "max"), Some("xhigh"));
    }

    #[test]
    fn parse_anthropic_response_drops_empty_text_blocks() {
        let body = serde_json::json!({
            "content": [
                {"type": "text", "text": ""},
                {"type": "text", "text": "   "},
                {
                    "type": "tool_use",
                    "id": "call_1",
                    "name": "reply",
                    "input": {"content": "hi"}
                }
            ],
            "usage": {"input_tokens": 1, "output_tokens": 2}
        });

        let response = parse_anthropic_response(body).expect("valid response");
        let contents: Vec<_> = response.choice.iter().collect();
        assert_eq!(contents.len(), 1);
        assert!(matches!(contents[0], AssistantContent::ToolCall(_)));
    }

    #[test]
    fn convert_messages_to_anthropic_omits_empty_text_messages() {
        let messages = OneOrMany::many(vec![
            Message::User {
                content: OneOrMany::one(UserContent::text("   ")),
            },
            Message::Assistant {
                id: None,
                content: OneOrMany::one(AssistantContent::text("")),
            },
            Message::Assistant {
                id: None,
                content: OneOrMany::one(AssistantContent::tool_call(
                    "call_1",
                    "reply",
                    serde_json::json!({"content": "ok"}),
                )),
            },
        ])
        .expect("non-empty message list");

        let converted = convert_messages_to_anthropic(&messages);
        assert_eq!(converted.len(), 1);
        assert_eq!(converted[0]["role"], "assistant");
        assert_eq!(converted[0]["content"][0]["type"], "tool_use");
    }

    #[test]
    fn end_turn_placeholder_is_not_forwarded_back_to_anthropic() {
        let body = serde_json::json!({
            "content": [],
            "stop_reason": "end_turn",
            "usage": {"input_tokens": 1, "output_tokens": 0}
        });

        let response = parse_anthropic_response(body).expect("valid response");
        let history = OneOrMany::one(Message::Assistant {
            id: None,
            content: response.choice,
        });

        let converted = convert_messages_to_anthropic(&history);
        assert!(converted.is_empty());
    }

    #[test]
    fn empty_non_end_turn_response_returns_error() {
        let body = serde_json::json!({
            "content": [],
            "stop_reason": "max_tokens",
            "usage": {"input_tokens": 1, "output_tokens": 0}
        });

        let error = parse_anthropic_response(body).expect_err("should fail");
        assert!(matches!(error, CompletionError::ResponseError(_)));
        assert!(error.to_string().contains("stop_reason: max_tokens"));
    }

    #[test]
    fn parse_openai_response_handles_content_array_parts() {
        let body = serde_json::json!({
            "choices": [{
                "message": {
                    "content": [
                        {"type": "text", "text": "First"},
                        {"type": "output_text", "text": "Second"}
                    ]
                },
                "finish_reason": "stop"
            }],
            "usage": {
                "prompt_tokens": 5,
                "completion_tokens": 3,
                "prompt_tokens_details": {"cached_tokens": 0}
            }
        });

        let response = parse_openai_response(body, "OpenRouter").expect("valid response");
        let texts: Vec<_> = response
            .choice
            .iter()
            .filter_map(|content| {
                if let AssistantContent::Text(text) = content {
                    Some(text.text.clone())
                } else {
                    None
                }
            })
            .collect();

        assert_eq!(texts, vec!["First".to_string(), "Second".to_string()]);
    }

    #[test]
    fn parse_openai_response_uses_reasoning_when_content_is_empty() {
        let body = serde_json::json!({
            "choices": [{
                "message": {
                    "content": "",
                    "reasoning": "Reasoning fallback output"
                },
                "finish_reason": "stop"
            }],
            "usage": {
                "prompt_tokens": 5,
                "completion_tokens": 1,
                "prompt_tokens_details": {"cached_tokens": 0}
            }
        });

        let response = parse_openai_response(body, "OpenRouter").expect("valid response");
        let first = response.choice.first_ref();
        match first {
            AssistantContent::Text(text) => assert_eq!(text.text, "Reasoning fallback output"),
            _ => panic!("expected text content"),
        }
    }

    #[test]
    fn parse_openai_response_handles_legacy_function_call() {
        let body = serde_json::json!({
            "choices": [{
                "message": {
                    "content": null,
                    "function_call": {
                        "name": "reply",
                        "arguments": "{\"content\":\"ok\"}"
                    }
                },
                "finish_reason": "function_call"
            }],
            "usage": {
                "prompt_tokens": 5,
                "completion_tokens": 1,
                "prompt_tokens_details": {"cached_tokens": 0}
            }
        });

        let response = parse_openai_response(body, "OpenRouter").expect("valid response");
        let first = response.choice.first_ref();
        match first {
            AssistantContent::ToolCall(tool_call) => {
                assert_eq!(tool_call.function.name, "reply");
                assert_eq!(tool_call.function.arguments["content"], "ok");
                assert!(!tool_call.id.is_empty());
            }
            _ => panic!("expected tool call"),
        }
    }

    #[test]
    fn parse_openai_response_empty_error_includes_provider_and_finish_reason() {
        let body = serde_json::json!({
            "choices": [{
                "message": {
                    "content": ""
                },
                "finish_reason": "stop"
            }],
            "usage": {
                "prompt_tokens": 5,
                "completion_tokens": 0,
                "prompt_tokens_details": {"cached_tokens": 0}
            }
        });

        let error = parse_openai_response(body, "OpenRouter").expect_err("should fail");
        assert!(error.to_string().contains("empty response from OpenRouter"));
        assert!(error.to_string().contains("finish_reason: stop"));
    }

    #[test]
    fn convert_messages_to_openai_tool_result_prefers_call_id_over_id() {
        let messages = OneOrMany::one(Message::User {
            content: OneOrMany::one(UserContent::ToolResult(rig::message::ToolResult {
                id: "legacy-id".to_string(),
                call_id: Some("stable-call-id".to_string()),
                content: OneOrMany::one(rig::message::ToolResultContent::text("ok")),
            })),
        });

        let converted = convert_messages_to_openai(&messages);
        assert_eq!(converted.len(), 1);
        assert_eq!(converted[0]["role"], "tool");
        assert_eq!(converted[0]["tool_call_id"], "stable-call-id");
    }

    #[test]
    fn convert_messages_to_openai_responses_function_call_output_prefers_call_id_over_id() {
        let messages = OneOrMany::one(Message::User {
            content: OneOrMany::one(UserContent::ToolResult(rig::message::ToolResult {
                id: "legacy-id".to_string(),
                call_id: Some("stable-call-id".to_string()),
                content: OneOrMany::one(rig::message::ToolResultContent::text("ok")),
            })),
        });

        let converted = convert_messages_to_openai_responses(&messages);
        assert_eq!(converted.len(), 1);
        assert_eq!(converted[0]["type"], "function_call_output");
        assert_eq!(converted[0]["call_id"], "stable-call-id");
    }

    #[test]
    fn convert_messages_to_openai_responses_function_call_prefers_call_id_over_id() {
        let messages = OneOrMany::one(Message::Assistant {
            content: OneOrMany::one(AssistantContent::ToolCall(ToolCall {
                id: "legacy-id".to_string(),
                call_id: Some("stable-call-id".to_string()),
                function: ToolFunction {
                    name: "reply".to_string(),
                    arguments: serde_json::json!({"content": "ok"}),
                },
                signature: None,
                additional_params: None,
            })),
            id: None,
        });

        let converted = convert_messages_to_openai_responses(&messages);
        assert_eq!(converted.len(), 1);
        assert_eq!(converted[0]["type"], "function_call");
        assert_eq!(converted[0]["call_id"], "stable-call-id");
    }

    #[test]
    fn parse_openai_responses_response_parses_fallback_text_without_output_text() {
        let body = serde_json::json!({
            "output": [{
                "type": "message",
                "content": [{
                    "type": "reasoning",
                    "summary": [
                        {"text": "step 1"},
                        {"text": "step 2"}
                    ]
                }]
            }],
            "usage": {
                "input_tokens": 3,
                "output_tokens": 2,
                "input_tokens_details": {"cached_tokens": 0}
            }
        });

        let response =
            parse_openai_responses_response(body, "OpenAI").expect("fallback text should parse");
        let texts: Vec<_> = response
            .choice
            .iter()
            .filter_map(|content| match content {
                AssistantContent::Text(text) => Some(text.text.clone()),
                _ => None,
            })
            .collect();

        assert_eq!(texts, vec!["step 1".to_string(), "step 2".to_string()]);
    }

    #[test]
    fn parse_openai_responses_response_preserves_function_call_call_id_from_completed_response() {
        let body = serde_json::json!({
            "output": [{
                "type": "function_call",
                "id": "legacy-id",
                "call_id": "stable-call-id",
                "name": "reply",
                "arguments": "{\"content\":\"ok\"}"
            }],
            "usage": {
                "input_tokens": 3,
                "output_tokens": 2,
                "input_tokens_details": {"cached_tokens": 0}
            }
        });

        let response =
            parse_openai_responses_response(body, "OpenAI").expect("function call should parse");
        match response.choice.first_ref() {
            AssistantContent::ToolCall(tool_call) => {
                assert_eq!(tool_call.id, "legacy-id");
                assert_eq!(tool_call.call_id.as_deref(), Some("stable-call-id"));
                assert_eq!(tool_call.function.name, "reply");
            }
            _ => panic!("expected tool call"),
        }
    }

    #[test]
    fn parse_openai_responses_response_unsupported_empty_error_is_actionable_and_provider_specific()
    {
        let body = serde_json::json!({
            "output": [{
                "type": "unknown_shape",
                "foo": "bar"
            }],
            "usage": {
                "input_tokens": 1,
                "output_tokens": 0,
                "input_tokens_details": {"cached_tokens": 0}
            }
        });

        let error =
            parse_openai_responses_response(body, "OpenAI").expect_err("should be unsupported");
        let error_text = error.to_string();
        assert!(error_text.contains("OpenAI Responses API"));
        assert!(error_text.contains("output_text/text/summary/refusal/content"));
        assert!(error_text.contains("unknown_shape"));
    }

    #[test]
    fn parse_openai_chat_sse_response_reconstructs_tool_calls() {
        let sse = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"Found \"},\"finish_reason\":null}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"files\",\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"function\":{\"name\":\"file\",\"arguments\":\"{\\\"operation\\\":\\\"list\\\"\"}}]},\"finish_reason\":null}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\",\\\"path\\\":\\\".\\\"}\"}}]},\"finish_reason\":\"tool_calls\"}]}\n\n",
            "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":12,\"completion_tokens\":8}}\n\n",
            "data: [DONE]\n\n"
        );

        let parsed = parse_openai_chat_sse_response(sse, "OpenRouter").expect("valid SSE");

        assert_eq!(parsed["choices"][0]["finish_reason"], "tool_calls");
        assert_eq!(parsed["choices"][0]["message"]["content"], "Found files");
        assert_eq!(
            parsed["choices"][0]["message"]["tool_calls"][0]["function"]["name"],
            "file"
        );
        assert_eq!(
            parsed["choices"][0]["message"]["tool_calls"][0]["function"]["arguments"]["operation"],
            "list"
        );
        assert_eq!(
            parsed["choices"][0]["message"]["tool_calls"][0]["function"]["arguments"]["path"],
            "."
        );
        assert_eq!(parsed["usage"]["prompt_tokens"], 12);
    }

    #[test]
    fn parse_openai_chat_sse_response_requires_data_lines() {
        let error = parse_openai_chat_sse_response("{\"choices\":[]}", "OpenRouter")
            .expect_err("should fail");
        assert!(error.to_string().contains("missing SSE data events"));
    }

    #[test]
    fn process_openai_chat_stream_event_flushes_tool_calls() {
        let first_event = serde_json::json!({
            "choices": [{
                "delta": {
                    "tool_calls": [{
                        "index": 0,
                        "id": "call_1",
                        "function": {
                            "name": "file",
                            "arguments": "{\"operation\":\"list\""
                        }
                    }]
                },
                "finish_reason": null
            }]
        });

        let second_event = serde_json::json!({
            "choices": [{
                "delta": {
                    "tool_calls": [{
                        "index": 0,
                        "function": {
                            "arguments": ",\"path\":\".\"}"
                        }
                    }]
                },
                "finish_reason": "tool_calls"
            }]
        });

        let mut pending = BTreeMap::new();

        let first_events = process_openai_chat_stream_event(&first_event, &mut pending)
            .expect("first event should parse");
        assert!(
            first_events.iter().any(|event| {
                matches!(
                    event,
                    RawStreamingChoice::ToolCallDelta {
                        content: rig::streaming::ToolCallDeltaContent::Name(name),
                        ..
                    } if name == "file"
                )
            }),
            "missing tool-call name delta"
        );
        assert_eq!(pending.len(), 1);

        let second_events = process_openai_chat_stream_event(&second_event, &mut pending)
            .expect("second event should parse");
        assert!(pending.is_empty(), "pending tool calls should be flushed");

        let tool_calls: Vec<_> = second_events
            .into_iter()
            .filter_map(|event| match event {
                RawStreamingChoice::ToolCall(tool_call) => Some(tool_call),
                _ => None,
            })
            .collect();
        assert_eq!(tool_calls.len(), 1);
        assert_eq!(tool_calls[0].name, "file");
        assert_eq!(tool_calls[0].arguments["operation"], "list");
        assert_eq!(tool_calls[0].arguments["path"], ".");
    }

    #[test]
    fn process_openai_chat_stream_event_does_not_duplicate_message_tool_calls() {
        let delta_event = serde_json::json!({
            "choices": [{
                "delta": {
                    "tool_calls": [{
                        "index": 0,
                        "id": "call_1",
                        "function": {
                            "name": "file",
                            "arguments": "{\"operation\":\"list\",\"path\":\".\"}"
                        }
                    }]
                },
                "finish_reason": null
            }]
        });

        let message_event = serde_json::json!({
            "choices": [{
                "message": {
                    "tool_calls": [{
                        "id": "call_1",
                        "function": {
                            "name": "file",
                            "arguments": "{\"operation\":\"list\",\"path\":\".\"}"
                        }
                    }]
                },
                "finish_reason": "tool_calls"
            }]
        });

        let mut pending = BTreeMap::new();
        let delta_events = process_openai_chat_stream_event(&delta_event, &mut pending)
            .expect("delta event should parse");
        let delta_internal_call_id = delta_events
            .iter()
            .find_map(|event| match event {
                RawStreamingChoice::ToolCallDelta {
                    internal_call_id, ..
                } => Some(internal_call_id.clone()),
                _ => None,
            })
            .expect("missing tool-call delta internal id");
        assert_eq!(pending.len(), 1);

        let events = process_openai_chat_stream_event(&message_event, &mut pending)
            .expect("message event should parse");
        let tool_calls: Vec<_> = events
            .into_iter()
            .filter_map(|event| match event {
                RawStreamingChoice::ToolCall(tool_call) => Some(tool_call),
                _ => None,
            })
            .collect();

        assert_eq!(tool_calls.len(), 1, "tool call should not be duplicated");
        assert_eq!(tool_calls[0].id, "call_1");
        assert_eq!(tool_calls[0].name, "file");
        assert_eq!(tool_calls[0].internal_call_id, delta_internal_call_id);
    }

    #[test]
    fn flush_openai_streaming_tool_calls_errors_on_invalid_arguments() {
        let mut pending = BTreeMap::new();
        pending.insert(
            0,
            OpenAiStreamingToolCall {
                id: "call_1".to_string(),
                internal_call_id: "internal_1".to_string(),
                name: "file".to_string(),
                arguments: "{\"operation\":\"list\"".to_string(),
            },
        );

        let error = flush_openai_streaming_tool_calls(&mut pending)
            .expect_err("invalid JSON arguments should error");
        assert!(
            error
                .to_string()
                .contains("invalid streamed tool arguments for 'file'"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn flush_openai_streaming_tool_calls_sanitizes_control_chars_in_string_arguments() {
        let mut pending = BTreeMap::new();
        pending.insert(
            0,
            OpenAiStreamingToolCall {
                id: "call_1".to_string(),
                internal_call_id: "internal_1".to_string(),
                name: "file".to_string(),
                arguments: "{\"operation\":\"write\",\"content\":\"line1\nline2\"}".to_string(),
            },
        );

        let events = flush_openai_streaming_tool_calls(&mut pending)
            .expect("control-char recovery should parse");
        let tool_calls: Vec<_> = events
            .into_iter()
            .filter_map(|event| match event {
                RawStreamingChoice::ToolCall(tool_call) => Some(tool_call),
                _ => None,
            })
            .collect();

        assert_eq!(tool_calls.len(), 1);
        assert_eq!(tool_calls[0].name, "file");
        assert_eq!(tool_calls[0].arguments["operation"], "write");
        assert_eq!(tool_calls[0].arguments["content"], "line1\nline2");
    }

    #[test]
    fn parse_openai_chat_sse_response_merges_multiline_data_blocks() {
        let sse = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"Hello\"},\n",
            "data: \"finish_reason\":null}]}\n\n",
            "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":2,\"completion_tokens\":1}}\n\n",
            "data: [DONE]\n\n"
        );

        let parsed = parse_openai_chat_sse_response(sse, "OpenRouter").expect("valid SSE");
        assert_eq!(parsed["choices"][0]["message"]["content"], "Hello");
        assert_eq!(parsed["usage"]["prompt_tokens"], 2);
    }

    #[test]
    fn extract_sse_data_payload_merges_data_lines() {
        let block = "event: message\nid: abc123\ndata: {\"a\":1}\ndata:{\"b\":2}";
        let payload = extract_sse_data_payload(block).expect("should parse data lines");
        assert_eq!(payload, "{\"a\":1}\n{\"b\":2}");
    }

    #[test]
    fn raw_streaming_response_reports_usage() {
        let usage = completion::Usage {
            input_tokens: 7,
            output_tokens: 3,
            total_tokens: 10,
            cached_input_tokens: 2,
        };

        let response = RawStreamingResponse {
            body: serde_json::json!({"ok": true}),
            usage: Some(usage),
        };

        assert_eq!(response.token_usage(), Some(usage));
    }

    #[test]
    fn format_api_error_includes_openrouter_metadata() {
        let status = reqwest::StatusCode::BAD_REQUEST;

        // OpenRouter-style error with provider_name and raw upstream error
        let body = serde_json::json!({
            "error": {
                "message": "Provider returned error",
                "metadata": {
                    "provider_name": "Moonshot",
                    "raw": "Invalid request: tool_use not supported"
                }
            }
        });
        let msg = format_api_error(status, &body);
        assert!(msg.contains("Provider returned error"));
        assert!(msg.contains("Moonshot"));
        assert!(msg.contains("tool_use not supported"));

        // Standard OpenAI error without metadata
        let body = serde_json::json!({
            "error": {
                "message": "Invalid model ID"
            }
        });
        let msg = format_api_error(status, &body);
        assert_eq!(msg, "400 Bad Request: Invalid model ID");

        // Metadata with provider_name but no raw
        let body = serde_json::json!({
            "error": {
                "message": "Provider returned error",
                "metadata": {
                    "provider_name": "Azure"
                }
            }
        });
        let msg = format_api_error(status, &body);
        assert!(msg.contains("Azure"));
        assert!(!msg.contains("raw"));

        // Structured JSON raw error (not a string)
        let body = serde_json::json!({
            "error": {
                "message": "Provider returned error",
                "metadata": {
                    "provider_name": "Google",
                    "raw": {"code": 400, "detail": "invalid schema"}
                }
            }
        });
        let msg = format_api_error(status, &body);
        assert!(msg.contains("Google"));
        assert!(msg.contains("invalid schema"));
    }
}
