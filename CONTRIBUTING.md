# Contributing to Spacebot

Thank you for contributing to Spacebot! This document outlines the development standards and workflows we follow to maintain code quality and collaboration efficiency.

## Development Philosophy

### Read Before Edit
Before modifying any code, take time to understand:
- The existing codebase structure and patterns
- The reasoning behind current implementations
- Dependencies and ripple effects of your changes

Read related documentation, issue discussions, and existing code comments. Understanding context prevents breaking changes and aligns your work with project goals.

### Grep for Usages
Before deleting or renaming any:
- Functions
- Modules
- Types
- Constants
- Configuration keys

Run `grep -r "your_identifier" .` to find all references. Update every usage, not just the definition. Unused code should be removed, but removal must be complete.

### Small, Reviewable PRs
Break larger features into focused, incremental changes:
- Each PR should address one logical concern
- Smaller PRs are easier to review, test, and debug if needed
- Complex features can be built from multiple approved smaller PRs

**Rule of thumb:** If your PR feels overwhelming to review, split it.

---

## Branch Hygiene

### Always Rebase
Before starting work or creating a PR:
```bash
git fetch upstream
git rebase upstream/main
```

This ensures your changes apply cleanly to the latest codebase and reduces merge conflicts.

### One Feature = One Branch = One PR
- Branches are for single features or bug fixes
- Don't mix unrelated changes in one branch
- One branch produces exactly one pull request
- After merge, delete the branch (local and remote)

### Branch Naming Convention
Use descriptive, lowercase branch names with hyphens:
```
feature/auth-oauth-flow
fix/memory-leak-in-worker
refactor/rename-cli-commands
docs/update-readme
```

Prefixes:
- `feature/` - New functionality
- `fix/` - Bug fixes
- `refactor/` - Code restructuring without behavior change
- `docs/` - Documentation updates
- `test/` - Test additions or improvements
- `chore/` - Build, tooling, or dependency changes

---

## Code Standards

### Pre-Commit Checklist

**1. `cargo check` MUST pass**
```bash
cargo check --all-features
```
This is your first gate. If `cargo check` fails, do not commit. Fix compilation errors before proceeding.

**2. Run tests for affected modules**
```bash
cargo test -p <affected_module>
```
Tests must pass before committing. If you modify core functionality, run the full test suite:
```bash
cargo test --all
```

**3. Format with `cargo fmt`**
```bash
cargo fmt --all
```
All code must match the project's formatting standards. This is non-negotiable—formatter output is the canonical style.

**4. Review your diff before commit**
```bash
git diff --staged
```
Ask yourself:
- Is every change intentional?
- Did I include debug code or commented-out code? (Remove it)
- Are my files properly formatted?
- Did I update documentation where needed?

Only commit when the diff is clean and intentional.

---

## PR Process

### Link PRs to Issues
Every PR must reference at least one issue:
- In the PR description, use `Closes #123` or `Fixes #456`
- If no issue exists, create one first—changes need context and approval before implementation
- Describe what the PR accomplishes and how it solves the problem

### Size Limits
Keep PRs focused:
- **Max 10 files** per PR
- **Max 500 lines** of changed code (additions + deletions)

If your PR exceeds these limits:
1. Split into multiple PRs with clear dependencies
2. Document the order in which they should be reviewed
3. Use PR comments to explain cross-PR dependencies

### Review Workflow

1. **Click First-Pass Review**
   - Initial review by a Click team member
   - Checks: alignment with requirements, architectural soundness, completeness
   - Request changes or approve

2. **CodeRabbit Review**
   - Automated code review by CodeRabbit
   - Provides additional analysis: security, performance, best practices
   - Address any flagged issues

3. **Click Final Review**
   - Second human review after CodeRabbit feedback is addressed
   - Final approval before merge
   - Ensures all concerns are resolved

4. **Merge**
   - Requires both human approvals
   - Squash merge to maintain clean git history
   - Delete the branch after merge

---

## Review Checklist

When reviewing a PR, verify:

### Code Safety
- [ ] **Does this delete existing code?**
  - If yes: Is there a clear justification? Every deletion must be explained and verified safe
  - Unused code should be removed, but only after confirming no active usages
  - Check for dependencies in other modules or features

### Initialization & Correctness
- [ ] **Are all new variables/functions initialized?**
  - No uninitialized variables
  - No `unwrap()` calls that could panic without justification
  - Proper error handling for fallible operations
  - Default values or constructors where appropriate

### Testing
- [ ] **Are tests included for new features?**
  - Unit tests for new functions
  - Integration tests for larger changes
  - Edge cases covered
  - Tests pass before PR approval

### Alignment
- [ ] **Does this trace to a company goal?**
  - Every change should support a documented objective
  - If the connection isn't obvious, it should be explained in the PR description
  - Prevents scope creep and ensures resources go to high-value work

### Style & Standards
- [ ] Does code pass `cargo check`, `cargo test`, and `cargo fmt`?
- [ ] Are imports organized and unused imports removed?
- [ ] Is documentation updated for API changes?
- [ ] Are changelog entries included for user-facing changes?

---

## Upstream Sync

### Weekly Sync
Spacebot is a fork. We must track upstream changes:

```bash
# Add upstream if not already configured
git remote add upstream <original_repo_url>

# Fetch latest
git fetch upstream

# Rebase main branch
git checkout main
git rebase upstream/main

# Push updated main
git push origin main
```

**Timing:** Sync at least weekly, preferably before Monday development starts.

### Tag Releases for Rollback
Before syncing upstream or before major merges:

```bash
# Create a tagged release point
git tag -a vYYYY.MM.DD -m "Pre-sync release checkpoint"

# Push tags to remote
git push origin --tags
```

**Tag format:** `vYYYY.MM.DD` (e.g., `v2026.03.10`)

This provides rollback capability if upstream changes introduce issues. Document any breaking upstream changes in project notes.

---

## 7. Rust Coding Standards

### Project Structure
- **Single binary crate** — No mod.rs files; all code lives at crate root or in leaf modules
- Flat structure preferred; deeply nested modules are discouraged

### Forbidden Patterns
- **`dbg!()`** — Never commit debugging macros
- **`todo!()`** — Use explicit error handling instead of leaving TODOs in production code
- **`unimplemented!()`** — Same as `todo!`; return an error or implement the feature

### Import Organization
Use 3-tier ordering, each tier sorted alphabetically:
```rust
// 1. Crate-local imports
use crate::auth::AuthConfig;
use crate::worker::Worker;

// 2. External crate imports
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

// 3. Standard library imports
use std::collections::HashMap;
use std::sync::Arc;
```

### Naming Conventions
- **Variables and functions:** `snake_case`
  - `fn get_user_by_id()`, `let user_count = 0`
- **Types, structs, enums:** `PascalCase`
  - `struct UserConfig`, `enum WorkerState`
- **Constants:** `SCREAMING_SNAKE_CASE`
  - `const MAX_RETRIES: u32 = 3;`
- **No abbreviations** — Write full words for clarity
  - `username` not `uname`, `index` not `idx`, `configuration` not `config`

### Error Handling
- Define a **top-level Error enum** with `#[from]` conversions:
  ```rust
  #[derive(Debug, thiserror::Error)]
  pub enum Error {
      #[from] Io(#[from] std::io::Error),
      #[from] Sql(#[from] sqlx::Error),
      #[from] Json(#[from] serde_json::Error),
  }
  ```
- **Never use `let _ =`** — Handle or explicitly ignore results with `let _ =` only when intentional
- Propagate errors with `?` operator
- Provide context with `.context()` when useful

### Async Patterns
- Use **native RPITIT** (Return Position Impl Trait In Trait):
  ```rust
  trait Worker {
      async fn run(&self) -> Result<(), Error>;
  }
  ```
- Do **not use `#[async_trait]`** unless interfacing with external libraries that require it
- Use **tokio** as the async runtime exclusively

### Database Layer
- **SQLite** — Relational data, migrations via sqlx
- **LanceDB** — Vector embeddings, similarity search
- **redb** — Key-value storage, low-latency lookups

---

## 8. Agent Development Rules

### JavaScript/TypeScript Tooling
- **Use `bun` exclusively** for JS/TS development
- **Never** use `npm`, `pnpm`, or `yarn`
- Install dependencies with `bun install`
- Run scripts with `bun run` or `bun <script>`

### Database Migrations
- **Never edit existing migrations** — They are immutable once applied
- Create new migrations to fix or extend schema changes
- Use `just preflight` to verify migration integrity

### Pre-Push Workflow
Before pushing any changes:
1. Run `just preflight` — Checks local linting, formatting, and test gates
2. Run `just gate-pr` — Validates PR readiness (includes preflight checks plus additional gates)

### Agent Behavior Rules
- **Don't block channels** — Workers should return quickly; use background tasks for long-running work
- **Don't dump raw search results** — Synthesize, summarize, and provide actionable insights
- **Workers don't get channel context** — Workers are stateless; all context must be passed explicitly in the task

### Tool Interaction Requirements
- **Tool nudging** — Guide users toward available tools when they request something that can be automated
- **Outcome gate required** — Workers MUST signal `set_status(kind: "outcome")` when work is complete before finishing

---

## 9. Pre-Commit Checklist

Before committing any changes, verify the following:

- [ ] **`cargo check` passes**
  ```bash
  cargo check --all-features
  ```

- [ ] **`cargo fmt` applied**
  ```bash
  cargo fmt --all
  ```

- [ ] **`just preflight` passes**
  ```bash
  just preflight
  ```

- [ ] **`just gate-pr` passes**
  ```bash
  just gate-pr
  ```

- [ ] **Diff reviewed**
  ```bash
  git diff --staged
  ```
  - Every change is intentional
  - No debug code or commented-out code
  - Files properly formatted
  - Documentation updated where needed

---

## Additional Resources

- **Issue Tracker:** Use issues for bug reports, feature requests, and questions
- **Documentation:** Keep docs updated alongside code changes
- **Communication:** Reach out in project channels for clarification or guidance

Following these standards ensures:
- High-quality, maintainable code
- Efficient review cycles
- Minimal regression risk
- Happy collaborators

Thank you for helping maintain Spacebot's quality and integrity!
