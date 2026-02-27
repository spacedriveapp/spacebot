---
name: async-state-safety
description: This skill should be used when the user asks to change "worker lifecycle", "cancellation", "retrigger behavior", "state machine", "delivery receipts", "timeouts", or "race conditions". Enforces explicit async/state invariants and targeted race-safe verification.
---

# Async State Safety

## Goal

Prevent race regressions in async and stateful paths.

## Required Invariants

- Define valid terminal states before coding.
- Define allowed state transitions before coding.
- Keep terminal transitions idempotent.
- Ensure duplicate events cannot double-apply terminal effects.
- Ensure retries do not corrupt state.

## Race Checklist

- Cancellation racing completion
- Timeout racing completion
- Retry racing ack/receipt update
- Concurrent updates to the same worker/channel record
- Missing-handle and stale-handle behavior

## Implementation Checklist

- Add or update transition guards.
- Keep error handling explicit and structured.
- Preserve status/event emission on all terminal branches.
- Document why each race path converges safely.

## Verification Checklist

- Run targeted tests for each touched race path.
- Add at least one negative-path test for terminal convergence.
- Add at least one idempotency test where applicable.
- Run broad gate checks after targeted checks pass.

## Handoff Requirements

- Terminal states and transition matrix
- Race windows analyzed
- Targeted commands and outcomes
- Residual risks and follow-up tests
