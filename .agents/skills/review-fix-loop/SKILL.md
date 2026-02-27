---
name: review-fix-loop
description: This skill should be used when the user asks to "address review feedback", "fix PR comments", "close findings", "respond to reviewer notes", or "reduce review churn". Enforces a finding-to-evidence closure loop for every review round.
---

# Review Fix Loop

## Goal

Close review rounds with minimal back-and-forth by mapping every finding to a code change and proof.

## Execution Loop

1. Parse findings into `P1`, `P2`, `P3`.
2. Build a closure table before editing.
3. Implement the smallest coherent fix batch.
4. Run targeted verification.
5. Update closure table with outcomes.
6. Run broader gate checks.

## Closure Table

Use one row per finding:

| Finding | Severity | Owned files | Planned change | Verification command | Result |
|---|---|---|---|---|---|

## Re-Run Control

- If the same command fails twice, stop rerunning.
- Isolate a smaller reproduction command.
- Patch the smallest likely cause.
- Re-run narrow verification before broad checks.

## Verification Ladder

- Start narrow: unit/module behavior checks for touched paths.
- Continue medium: compile/lint/type checks for touched surfaces.
- End broad: repository gate commands.

## Handoff Requirements

- Findings closed
- Commands executed
- Pass/fail evidence per finding
- Remaining open findings with rationale
- Residual risk
