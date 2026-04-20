# src/hooks/

Prompt hook implementations for all LLM process types.

## Files

- `spacebot.rs` (1614 lines) — `SpacebotHook`: channels, branches, workers
- `cortex.rs` — `CortexHook`: cortex process observation
- `loop_guard.rs` (788 lines) — `LoopGuard`: repetition detection

## SpacebotHook

Implements Rig's `PromptHook`. Called on every LLM turn.

- Sends `ProcessEvent`s: status reporting, usage tracking, cancellation signals
- Returns `Continue`, `Terminate`, or `Skip`
- Runs leak detection in `on_tool_result()` (see below)

**Tool nudging:** Workers must call `set_status(kind: "outcome")` before exiting with text. If a worker returns text without an outcome signal:
1. Hook fires `Terminate` + injects nudge prompt
2. Retries up to 2 times
3. After retries exhausted → `PromptCancelled`

## CortexHook

Lightweight. Observes cortex turns, tracks usage. No nudging, no loop guard.

## LoopGuard (`loop_guard.rs`)

Detects repetitive LLM outputs to prevent infinite tool-call loops.

- Configurable via `LoopGuardConfig` (window size, similarity threshold, max repeats)
- Fires `Terminate` when repetition detected
- Applied to channels and workers; not cortex

## Leak Detection

Runs in `SpacebotHook::on_tool_result()` after every tool execution.

- Regex patterns: API keys, bearer tokens, PEM keys, common secret formats
- Match → redact from result before it reaches LLM context
- Blocks exfiltration via outbound HTTP tool calls
