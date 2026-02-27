//! Provider client initialization.

use crate::config::LlmConfig;
use crate::error::Result;

/// Initialize all configured provider clients.
pub async fn init_providers(config: &LlmConfig) -> Result<()> {
    // Provider clients are initialized lazily through LlmManager
    // This module exists for any provider-specific setup that needs to happen
    // during system startup

    if config.anthropic_key.is_some() {
        tracing::info!("Anthropic provider configured (API key)");
    }

    if config.openai_key.is_some() {
        tracing::info!("OpenAI provider configured");
    }

    if config.kilo_key.is_some() {
        tracing::info!("Kilo Gateway provider configured");
    }

    if config.ollama_base_url.is_some() || config.ollama_key.is_some() {
        tracing::info!("Ollama provider configured");
    }

    if config.opencode_zen_key.is_some() {
        tracing::info!("OpenCode Zen provider configured");
    }

    if config.opencode_go_key.is_some() {
        tracing::info!("OpenCode Go provider configured");
    }

    if config.minimax_key.is_some() {
        tracing::info!("MiniMax provider configured");
    }

    if config.moonshot_key.is_some() {
        tracing::info!("Moonshot AI provider configured");
    }

    if config.nvidia_key.is_some() {
        tracing::info!("NVIDIA provider configured");
    }

    if config.gemini_key.is_some() {
        tracing::info!("Google Gemini provider configured");
    }

    Ok(())
}
