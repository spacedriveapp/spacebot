---
name: pr-slicer
description: This skill should be used when the user asks to "split this PR", "make this smaller", "create stacked PRs", "slice this change", or "reduce review churn". Helps break work into low-risk, reviewable slices with clear verification per slice.
---

# PR Slicer

## Goal

Reduce review latency and rework by shipping smaller, independent slices.

## Default Slice Budgets

- Target `<= 400` changed lines per slice when practical.
- Target `<= 10` changed files per slice when practical.
- Target `1-4` commits per slice.
- Keep each slice behaviorally coherent and independently verifiable.

## Slicing Order

1. Extract prerequisites first.
2. Land mechanical refactors next.
3. Land behavior changes after prerequisites are merged.
4. Land UI/docs/polish last.

## Slice Packet Template

For each slice, define:

- `Goal`
- `Owned files`
- `Out of scope`
- `Risk level` (`low`/`medium`/`high`)
- `Verification command(s)` with expected pass condition
- `Rollback plan`

## Hard Rules

- Avoid mixing refactor and behavior changes in one slice unless unavoidable.
- Avoid touching unrelated subsystems in one slice.
- Avoid cross-slice hidden dependencies.
- If a slice depends on unmerged work, state it explicitly.

## Verification Discipline

- Run narrow checks first for touched behavior.
- Run project gate checks before handoff.
- Record exact commands and outcomes for each slice.

## Final Handoff Format

- Slice list with order and purpose
- Per-slice owned files
- Per-slice verification evidence
- Residual risk and follow-up slices
