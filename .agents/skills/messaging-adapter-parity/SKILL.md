---
name: messaging-adapter-parity
description: This skill should be used when the user asks to change "Slack adapter", "Telegram adapter", "Discord adapter", "Webhook adapter", "status delivery", "message routing", or "delivery receipts". Enforces cross-adapter parity and explicit delivery semantics.
---

# Messaging Adapter Parity

## Goal

Prevent adapter-specific regressions by validating behavior contracts across messaging backends.

## Contract Areas

- User-visible reply behavior
- Status update behavior (`surfaced` vs `not surfaced`)
- Delivery receipt ack/failure semantics
- Retry behavior and bounded backoff
- Error mapping and logging clarity

## Parity Checklist

- For every changed adapter path, compare expected behavior with at least one other adapter.
- If an adapter intentionally does not surface a status, ensure receipt handling still converges correctly.
- Ensure unsupported features degrade gracefully and predictably.
- Ensure worker terminal notices cannot loop indefinitely.

## Verification Checklist

- Run targeted tests for the touched adapter.
- Run targeted tests for receipt ack/failure paths.
- Run at least one parity comparison check across adapters.
- Run broad gate checks after targeted checks pass.

## Required Handoff

- Adapter paths changed
- Contract decisions made
- Receipt behavior outcomes
- Verification evidence
- Residual parity gaps
