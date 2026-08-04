---
name: deep-review
description: This skill should be used when the user asks for a "deep review", "thorough review", "multi-agent review", "full review", or "review this PR/branch/diff" beyond a quick pass. Fans out parallel specialist reviewer agents (security, logic, conformity, quality, tests, contracts, docs, UI) over a shared change map and synthesizes one deduplicated, severity-ranked report.
---

# Deep Review

Multi-agent code review. You are the orchestrator: you build the change map, fan out dedicated reviewer agents in one batch, and own the synthesized report. Reviewers advise; you decide.

The reviewers are project agents defined in `.omp/agents/review-*.md`. Each is read-only, carries its own area brief and severity contract, and returns structured findings. Read an agent file to see exactly what a reviewer checks.

## Principles

- Reviewers NEVER edit code or run builds/gates/test suites. Their agent definitions enforce this; repeat it in the batch context.
- Fan out exactly as wide as the change surface justifies. Never pad the batch with areas the change does not touch.
- Subagents start blank. Each task item carries the change map (or a `local://` pointer to it) and its specific focus.

## Severity Scheme

| Severity | Meaning | Examples |
|---|---|---|
| P1 | Must fix before merge | Correctness bug, security hole, data loss, breaking change, race condition |
| P2 | Should fix | New contract without a test, convention violation with teeth, real maintainability debt |
| P3 | Optional | Nits, style preferences, speculative improvements |

Every P1/P2 finding must include a targeted verification command.

## Reviewer Roster

Core reviewers run on every review. Conditional reviewers run only when their trigger surface appears in the change map.

| Name | Agent | Trigger |
|---|---|---|
| SecurityReview | `review-security` | always |
| LogicReview | `review-logic` | always |
| ConformityReview | `review-conformity` | always |
| QualityReview | `review-quality` | always |
| TestReview | `review-tests` | always |
| ContractReview | `review-contracts` | public API, migration, config, or wire/event surface changed |
| DocsReview | `review-docs` | user-facing behavior, config, or feature changed |
| UiReview | `designer` (bundled) | UI/frontend files changed; task: point at changed UI files, ask for visual/UX/accessibility review |

## Workflow

### Phase 0 — Map the change surface (you, inline; never delegated)

Establish WHAT is under review before spawning anyone:

- PR: read `pr://<N>` for intent and discussion; diff base..head.
- Branch: `git log <base>..HEAD --oneline` and `git diff <base>...HEAD --stat`, then the full diff.
- Working tree: `git status` + `git diff`.

Build the change map:

1. Changed files grouped by subsystem.
2. Exported symbols added/changed/removed.
3. Callers of changed exported symbols (`lsp references`).
4. Surface triggers: migrations? public API? config/env? UI? user-facing behavior?

Write the map to `local://change-map.md` if it exceeds ~50 lines. Decide which roster areas trigger. Skipping an area requires a stated reason in the final report.

### Phase 1 — Fan out (exactly one `task` batch call)

Spawn ALL triggered reviewers in a SINGLE `tasks[]` batch, each with its roster `agent`. Never serialize reviewers across multiple calls.

Shared `context`:

```
# Goal
Deep review of <target>: <one-paragraph intent of the change>.
# Constraints
- READ-ONLY: no edits, no writes, no builds, no test-suite or gate runs.
- Report only what the diff touches or directly affects.
# Contract
- Change map: <inline map, or "read local://change-map.md">
- Return your structured findings per your output schema; severity per P1/P2/P3; P1/P2 include a verification command.
```

Each task item:

```
# Target
<files and symbols this reviewer owns, from the change map>
# Focus
<change-specific pointers: which hunks matter most for this area, cross-area boundaries to respect>
```

Attach this `outputSchema` to every item:

```json
{
  "type": "object",
  "required": ["summary", "findings"],
  "properties": {
    "summary": { "type": "string", "description": "2-3 sentence area verdict" },
    "findings": {
      "type": "array",
      "items": {
        "type": "object",
        "required": ["severity", "confidence", "title", "location", "evidence", "recommendation"],
        "properties": {
          "severity": { "enum": ["P1", "P2", "P3"] },
          "confidence": { "enum": ["high", "medium", "low"] },
          "title": { "type": "string" },
          "location": { "type": "string", "description": "file:line" },
          "evidence": { "type": "string", "description": "quoted code + why it is wrong" },
          "recommendation": { "type": "string" },
          "verification": { "type": "string", "description": "targeted command; required for P1/P2" }
        }
      }
    }
  }
}
```

A reviewer that finds nothing returns empty `findings`. That is a valid result — do not respawn to force findings.

### Phase 2 — Synthesize (you)

1. Collect structured outputs from all reviewers.
2. Dedupe: same root cause from multiple areas → one finding, highest severity, note all reporting areas.
3. Spot-check every P1 by reading the cited code yourself. A false P1 erodes the report's trust.
4. Contradictions: reviewers stay `idle` after yielding — message them via `hub` (`send` with `await: true`) instead of respawning.
5. Drop low-confidence P3s unless they corroborate another finding.

### Phase 3 — Report

```
## Deep Review: <target>

### Verdict
merge | merge after P1 fixes | do not merge — one-sentence justification

### Findings
| # | Severity | Area | Location | Finding | Recommendation | Verification |
(severity-ordered, deduplicated)

### Coverage
Areas run / skipped + reasons. Reviewer disagreements and how resolved.

### Residual Risk
What static review cannot see: runtime behavior, external systems, perf under load.

### Open Questions
Low-confidence items and judgment calls needing a human.
```

## Guardrails

- NEVER delegate Phase 0 or Phase 2 — decomposition and adjudication stay with you.
- NEVER spawn a second wave to re-review covered ground; use `hub` follow-ups with the idle reviewers.
- If the change surface is tiny (< ~3 files, no API/schema/security surface), say so and review it yourself instead of fanning out.

## Model Routing (optional)

Reviewer agents inherit the session model by default. For a dedicated review model, add `model: "@review"` to each `.omp/agents/review-*.md` frontmatter and set `modelRoles.review` in `~/.omp/agent/config.yml`.
