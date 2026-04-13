---
name: commit-all
description: Use this skill when the user asks to "commit all", "commit everything", or wants all outstanding changes committed. Groups unrelated changes into separate, well-described commits instead of one catch-all commit.
---

# Commit All

## Goal

Commit every outstanding change in the working tree — but group unrelated changes into separate, informative commits so the git history stays useful.

## Workflow

1. **Survey all changes.** Run `git status` and `git diff` (staged + unstaged) to see the full picture. Include untracked files.
2. **Identify logical groups.** Cluster files by the change they belong to. A "group" is a set of files that were modified for the same reason (e.g. a bug fix, a new feature, a config tweak, a dependency update). Use file paths, diff content, and your understanding of the codebase to decide.
3. **Order commits.** Infra/config/dependency changes first, then library/core changes, then feature/UI changes, then docs/polish.
4. **For each group, create one commit:**
   - Stage only the files belonging to that group (`git add <file> ...`). Never use `git add -A` or `git add .`.
   - Write a concise, informative commit message that describes *what* changed and *why*. Follow the repo's existing commit style (check `git log --oneline -10`).
   - Do not lump unrelated changes together just because they're small.
5. **Verify.** After all commits, run `git status` to confirm the tree is clean. Run `git log --oneline -n <N>` (where N = number of commits created) to show the user what was committed.

## Commit Message Rules

- Keep the subject line under 72 characters.
- Use imperative mood ("add", "fix", "update", not "added", "fixes").
- If a change is trivial (whitespace, typo, formatting), it's fine to batch those into one commit labeled accordingly.
- End every commit message with the Co-Authored-By trailer.

## Hard Rules

- Never combine unrelated changes in one commit.
- Never skip or discard changes — everything gets committed.
- Never use `git add -A` or `git add .`.
- Do not push. Only commit locally.
- Do not commit files that look like they contain secrets (`.env`, credentials, tokens). Warn the user about those instead.
