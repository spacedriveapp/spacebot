# src/llm/ — LLM Routing Subsystem

## What This Is

All LLM calls flow through here. No Rig built-in provider clients. Every agent uses
`SpacebotModel`, which wraps `LlmManager` for routing, fallback, and rate limiting.

## Files

**`model.rs`** (critical hotspot, 3300+ lines)
Custom `CompletionModel` impl. Routes every call through `LlmManager`. Handles streaming,
tool call formatting, and provider-specific payload differences. Touch carefully.

**`manager.rs`**
`LlmManager` holds all provider clients. Resolves model names to providers. Manages
fallback chains. Tracks 429s and deprioritizes providers for a configurable cooldown.

**`routing.rs`**
`RoutingConfig` — four-level routing:
1. Process-type defaults (channel → sonnet, worker → haiku)
2. Task-type overrides (coding → sonnet)
3. Prompt complexity scoring (light / standard / heavy)
4. Fallback chains

Complexity scoring runs on user messages only, not system prompts. Sub-millisecond, no
external calls.

**`providers.rs`**
Client init for 15+ providers: Anthropic, OpenAI, OpenRouter, Kilo, Groq, Together,
Fireworks, DeepSeek, xAI, Mistral, NVIDIA, MiniMax, Moonshot, OpenCode Zen/Go, Ollama,
Z.ai, and custom endpoints.

**`pricing.rs`**
Model pricing data used for usage tracking and cost attribution.

## anthropic/ Subdirectory

Anthropic-specific formatting and features, split into four files:

- **`messages.rs`** — Messages API request/response formatting
- **`streaming.rs`** — SSE stream parsing
- **`cache.rs`** — Prompt caching via ephemeral `cache_control` blocks
- **`extended_thinking.rs`** — Extended thinking support

## Key Invariants

- `SpacebotModel` is the only `CompletionModel` impl. Don't add another.
- All provider clients live in `LlmManager`. Nothing else holds provider state.
- Routing is pure and fast. No I/O, no async, no side effects in the scoring path.
- Rate-limited providers are deprioritized, not dropped. Fallback chains handle the rest.
- `model.rs` is a hotspot. Every LLM call passes through it. Profile before refactoring.
