---
name: release-bump-changelog
description: Use this skill when preparing a release bump or updating release notes. It writes a launch-style release story from the actual change set, then runs `cargo bump` so the generated GitHub notes and the marketing copy land together in `CHANGELOG.md`.
---

# Release Bump + Changelog

## Goal

Create a version bump commit where each release section includes both:

- a launch-style narrative (marketing copy)
- the exact GitHub-generated release notes

## Workflow

1. Ensure the working tree is clean (except allowed release files).
2. Draft release story markdown from real changes (PR titles, release-note bullets, and diff themes).
   - Target style: similar to the `v0.2.0` narrative (clear positioning + concrete highlights).
   - Keep it factual and specific to the release.
   - Write to a temp file (outside repo is preferred):
     - `marketing_file="$(mktemp)"`
     - write markdown content to `$marketing_file`
3. Run `cargo bump <patch|minor|major|X.Y.Z>` with marketing copy input:
   - `SPACEBOT_RELEASE_MARKETING_COPY_FILE="$marketing_file" cargo bump <...>`
   - This invokes `scripts/release-tag.sh`.
   - The script generates GitHub-native notes (`gh api .../releases/generate-notes`).
   - The script upserts `CHANGELOG.md` with:
     - `### Release Story` (from your marketing file)
     - GitHub-generated notes body
   - The script includes `CHANGELOG.md` in the release commit.
4. Verify results:
   - `git show --name-only --stat`
   - Confirm commit contains `Cargo.toml`, `Cargo.lock` (if present), and `CHANGELOG.md`.
   - Confirm tag was created (`git tag --list "v*" --sort=-v:refname | head -n 5`).

## Requirements

- `gh` CLI installed and authenticated (`gh auth status`).
- `origin` remote points to GitHub, or set `SPACEBOT_RELEASE_REPO=<owner/repo>`.
- Marketing copy is required unless explicitly bypassed with `SPACEBOT_SKIP_MARKETING_COPY=1`.

## Release Story Format

Use markdown only (no outer `## vX.Y.Z` heading; script adds it). Recommended structure:

1. One strong opening paragraph (why this release matters)
2. One paragraph on major technical shifts
3. Optional short highlight bullets for standout additions/fixes

Avoid vague hype. Tie claims to concrete shipped changes.

## Notes

- Do not use a standalone changelog sync script.
- `CHANGELOG.md` is seeded from historical releases and then maintained by the release bump workflow.
