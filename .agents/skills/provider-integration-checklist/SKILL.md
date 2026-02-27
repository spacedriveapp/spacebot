---
name: provider-integration-checklist
description: This skill should be used when the user asks to add or modify an "LLM provider", "model routing", "OAuth flow", "auth token handling", "provider config", or "fallback chain". Enforces provider integration completeness across config, routing, docs, and verification.
---

# Provider Integration Checklist

## Goal

Ship provider and routing changes without regressions in auth, config, and model selection.

## Change Checklist

- Add config keys and defaults in one place.
- Validate config resolution order (`env > DB > default` where applicable).
- Validate model identifier parsing and normalization.
- Validate routing defaults, task overrides, and fallback chains.
- Validate auth flow behavior for both success and failure paths.
- Validate token/secret handling and redaction behavior.

## Compatibility Checklist

- Keep existing providers unaffected.
- Keep unknown-provider errors actionable.
- Keep provider-specific errors distinguishable.
- Keep docs and examples aligned with actual config keys.

## Verification Checklist

- Narrow tests for changed provider/routing/auth paths.
- Negative-path tests for invalid config and auth failures.
- Smoke path proving model call routes to expected backend.
- Broad gate checks after targeted checks pass.

## Required Handoff

- Config keys changed
- Routing behavior changed
- Auth behavior changed
- Verification evidence
- Docs updated
