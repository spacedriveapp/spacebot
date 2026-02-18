//! Provider client initialization.

use crate::config::LlmConfig;
use crate::error::Result;

/// Initialize all configured provider clients.
pub async fn init_providers(config: &LlmConfig) -> Result<()> {
    // Provider clients are initialized lazily through LlmManager
    // This module exists for any provider-specific setup that needs to happen
    // during system startup

    if config.anthropic_key.is_some() {
        tracing::info!("Anthropic provider configured");
    }

    if config.openai_key.is_some() {
        tracing::info!("OpenAI provider configured");
    }

    if config.openrouter_key.is_some() {
        tracing::info!("OpenRouter provider configured");
    }

    if config.ollama_key.is_some() {
        tracing::info!("Ollama provider configured");
    }

    if config.opencode_zen_key.is_some() {
        tracing::info!("OpenCode Zen provider configured");
    }

    Ok(())
}
